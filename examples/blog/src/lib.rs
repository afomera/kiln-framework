//! Example app: a blog with users, posts and comments.

pub mod actions;
pub mod app;
pub mod config;
pub mod factories;
pub mod forms;
pub mod mail;
pub mod ops;
pub mod parts;
pub mod policies;
pub mod records;
pub mod seed;
pub mod views;

pub use seed::seed;
