#![allow(dead_code)]

fn byte_literal() -> u8 { b'a' }
fn after_byte() -> u8 { b'z' }
fn borrowed_identifier(raw: Vec<u8>) -> usize { (&raw).len() }
fn with_closure(n: i32) -> i32 {
    let classify = |x| if x > 0 { 1 } else { 0 };
    classify(n)
}
fn bounded<T: Copy>(value: T) -> T { value }
macro_rules! branch { ($x:expr) => { if $x > 0 { 1 } else { 0 } }; }
fn through_macro(n: i32) -> i32 { branch!(n) }

#[cfg(all(test, unix))]
mod checks {
    fn helper() -> u8 { b'h' }
    #[test]
    fn unit_check() { assert_eq!(super::byte_literal(), b'a'); }
}

#[cfg(any(test, feature = "optional"))]
fn mixed_cfg() -> bool { true }

#[cfg(not(test))]
fn only_production() -> bool { false }

#[test]
fn standalone_test() { assert_eq!(after_byte(), b'z'); }
