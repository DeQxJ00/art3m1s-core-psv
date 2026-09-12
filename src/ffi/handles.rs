//! Generational handle table for the versioned FFI facades.
//!
//! Facade handles are `(generation << 32) | (slot index + 1)`; 0 is never a
//! valid handle. Every facade call resolves the handle through the table lock,
//! so garbage, stale, or double-destroyed handles are rejected as
//! `INVALID_HANDLE` instead of dereferencing raw pointer bits, and concurrent
//! facade calls serialize on the table instead of aliasing `&mut` runtime
//! state. The lock is only held for lookup/removal plus the engine call; value
//! destruction happens after the lock is released so destructor-side logging
//! cannot deadlock against host callbacks that re-enter the facade.

use std::sync::Mutex;

struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

pub struct HandleTable<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
}

impl<T> HandleTable<T> {
    pub const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }

    pub fn insert(&mut self, value: T) -> u64 {
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.generation = next_generation(slot.generation);
            slot.value = Some(value);
            encode_handle(index, slot.generation)
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(Slot {
                generation: 1,
                value: Some(value),
            });
            encode_handle(index, 1)
        }
    }

    pub fn get(&self, handle: u64) -> Option<&T> {
        let (index, generation) = decode_handle(handle)?;
        let slot = self.slots.get(index as usize)?;
        if slot.generation != generation {
            return None;
        }
        slot.value.as_ref()
    }

    pub fn get_mut(&mut self, handle: u64) -> Option<&mut T> {
        let (index, generation) = decode_handle(handle)?;
        let slot = self.slots.get_mut(index as usize)?;
        if slot.generation != generation {
            return None;
        }
        slot.value.as_mut()
    }

    /// Removes and returns the value; the slot generation is left unchanged so
    /// the next insert bumps it and stale handles stay invalid.
    pub fn remove(&mut self, handle: u64) -> Option<T> {
        let (index, generation) = decode_handle(handle)?;
        let slot = self.slots.get_mut(index as usize)?;
        if slot.generation != generation {
            return None;
        }
        let value = slot.value.take()?;
        self.free.push(index);
        Some(value)
    }
}

fn next_generation(generation: u32) -> u32 {
    let next = generation.wrapping_add(1);
    if next == 0 { 1 } else { next }
}

fn encode_handle(index: u32, generation: u32) -> u64 {
    ((generation as u64) << 32) | (index as u64 + 1)
}

fn decode_handle(handle: u64) -> Option<(u32, u32)> {
    if handle == 0 {
        return None;
    }
    let index = (handle & 0xFFFF_FFFF) as u32;
    if index == 0 {
        return None;
    }
    Some((index - 1, (handle >> 32) as u32))
}

pub struct LockedHandleTable<T> {
    table: Mutex<HandleTable<T>>,
}

impl<T> LockedHandleTable<T> {
    pub const fn new() -> Self {
        Self {
            table: Mutex::new(HandleTable::new()),
        }
    }

    pub fn insert(&self, value: T) -> u64 {
        self.table
            .lock()
            .map(|mut table| table.insert(value))
            .unwrap_or(0)
    }

    /// Runs `f` with the resolved value while holding the table lock, which
    /// serializes facade calls against each other and against `destroy`.
    pub fn with<R>(&self, handle: u64, invalid: R, f: impl FnOnce(&T) -> R) -> R
    where
        R: Default,
    {
        let Ok(table) = self.table.lock() else {
            return invalid;
        };
        match table.get(handle) {
            Some(value) => f(value),
            None => invalid,
        }
    }

    pub fn with_mut<R>(&self, handle: u64, invalid: R, f: impl FnOnce(&mut T) -> R) -> R
    where
        R: Default,
    {
        let Ok(mut table) = self.table.lock() else {
            return invalid;
        };
        match table.get_mut(handle) {
            Some(value) => f(value),
            None => invalid,
        }
    }

    /// Removes the value under the lock and returns it so the caller can run
    /// teardown (native destroy, destructors that may log or re-enter the
    /// facade) after the lock is released.
    pub fn take(&self, handle: u64) -> Option<T> {
        self.table
            .lock()
            .ok()
            .and_then(|mut table| table.remove(handle))
    }

    pub fn destroy(&self, handle: u64) -> bool {
        self.take(handle).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserted_handles_resolve_and_zero_is_invalid() {
        let mut table = HandleTable::new();
        let handle = table.insert(String::from("runtime"));
        assert_ne!(handle, 0);
        assert_eq!(table.get(handle).map(String::as_str), Some("runtime"));
        assert!(table.get(0).is_none());
    }

    #[test]
    fn garbage_and_foreign_handles_are_rejected() {
        let mut table = HandleTable::new();
        table.insert(1u32);
        assert!(table.get(u64::MAX).is_none());
        assert!(table.get(1 << 32).is_none()); // generation 1, index bits 0
        assert!(table.get(2).is_none()); // slot 1 was never allocated
    }

    #[test]
    fn stale_handle_stays_invalid_after_slot_reuse() {
        let mut table = HandleTable::new();
        let first = table.insert("a");
        let second = table.insert("b");
        assert_eq!(table.remove(first), Some("a"));
        assert!(table.get(first).is_none());
        assert!(table.remove(first).is_none()); // double destroy

        let recycled = table.insert("c");
        assert_ne!(recycled, first);
        assert_eq!(table.get(recycled), Some(&"c"));
        assert_eq!(table.get(second), Some(&"b"));
    }

    #[test]
    fn locked_table_serializes_access() {
        static TABLE: LockedHandleTable<u32> = LockedHandleTable::new();
        let handle = TABLE.insert(7);
        assert_eq!(TABLE.with(handle, 0, |value| *value), 7);
        assert_eq!(
            TABLE.with_mut(handle, 0, |value| {
                *value += 1;
                *value
            }),
            8
        );
        assert!(TABLE.destroy(handle));
        assert!(!TABLE.destroy(handle));
        assert_eq!(TABLE.with(handle, 0, |value| *value), 0);
    }
}
