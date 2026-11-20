//! Sample module
use std::collections::HashMap;

/// A point.
#[derive(Debug, Clone)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

fn main() {
    let mut map: HashMap<String, Point> = HashMap::new();
    map.insert("origin".to_string(), Point::new(0, 42));
    // print it
    println!("{:?}", map);
}
