//! Example app: a blog with users, posts and comments.

pub mod actions;
pub mod app;
pub mod factories;
pub mod forms;
pub mod ops;
pub mod parts;
pub mod policies;
pub mod records;
pub mod seed;
pub mod views;

pub use seed::seed;
