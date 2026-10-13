// SPDX-FileCopyrightText: 2026 roolrz
// SPDX-License-Identifier: Apache-2.0

//! Pipeline readiness can exceed one wait-many call's object limit. Large sets
//! subscribe in bounded groups; the outer wait observes the groups' readiness.

use hyper_os::handle::{ByteChannelObject, HandleRef, ProcessObject, WaitSetObject};
use hyper_os::wait::{MAX_ITEMS, ObjectSignals, WaitItem, WaitSet, wait_many};

pub enum Source<'a> {
    Channel(
        HandleRef<'a, ByteChannelObject>,
        ObjectSignals<ByteChannelObject>,
    ),
    Process(HandleRef<'a, ProcessObject>),
}

impl Source<'_> {
    fn item(&self) -> WaitItem<'_> {
        match self {
            Self::Channel(handle, signals) => WaitItem::new(*handle, *signals),
            Self::Process(handle) => {
                WaitItem::new(*handle, ObjectSignals::<ProcessObject>::TERMINATED)
            }
        }
    }
    fn register(&self, set: &WaitSet) -> hyper_os::Result<()> {
        match self {
            Self::Channel(handle, signals) => set.add(*handle, *signals)?,
            Self::Process(handle) => {
                set.add(*handle, ObjectSignals::<ProcessObject>::TERMINATED)?
            }
        };
        Ok(())
    }
}

pub fn wait(sources: &[Source<'_>]) -> hyper_os::Result<()> {
    if sources.len() <= MAX_ITEMS {
        let items: Vec<_> = sources.iter().map(Source::item).collect();
        wait_many(&items, hyper_os::DEADLINE_INFINITE)?;
    } else {
        if sources.len() > MAX_ITEMS * MAX_ITEMS {
            return Err(hyper_os::Error::InvalidWaitSet);
        }
        let mut groups = Vec::new();
        for chunk in sources.chunks(MAX_ITEMS) {
            let set = WaitSet::new(chunk.len())?;
            for source in chunk {
                source.register(&set)?;
            }
            groups.push(set);
        }
        let items: Vec<_> = groups
            .iter()
            .map(|set| {
                WaitItem::new(
                    set.as_handle_ref(),
                    ObjectSignals::<WaitSetObject>::READABLE,
                )
            })
            .collect();
        wait_many(&items, hyper_os::DEADLINE_INFINITE)?;
        // Dropping the groups unregisters every source before the reactor may
        // retire a handle. A level present before registration stays observable.
    }
    Ok(())
}
