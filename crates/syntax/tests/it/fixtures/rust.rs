//! Crate docs.
use std::collections::HashMap;

const MAX_ITEMS: usize = 16;

/// A pair of values.
#[derive(Debug, Clone)]
pub struct Pair<T> {
    left: T,
    right: Option<T>,
}

impl<T: Clone> Pair<T> {
    pub fn new(left: T) -> Self {
        Self { left, right: None }
    }

    fn swap(&mut self, other: &str) -> Result<usize, String> {
        let n = other.len() as u32 + 0x1f;
        for i in 0..MAX_ITEMS {
            if i % 2 == 0 && other.starts_with("a\n") {
                println!("{} {}", i, 3.5);
            }
        }
        match self.right.take() {
            Some(v) => Ok(n as usize),
            None => Err(format!("none")),
        }
    }
}
