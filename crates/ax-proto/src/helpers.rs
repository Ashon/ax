//! Serde helpers that reproduce `json:",omitempty"` behaviour for
//! concrete field types. Because serde has no generic "skip if zero" we
//! expose one predicate per primitive used in the protocol.

#[inline]
pub fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

#[inline]
pub fn is_false(v: &bool) -> bool {
    !*v
}
