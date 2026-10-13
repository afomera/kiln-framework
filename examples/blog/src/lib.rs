//! Example app: a blog with users, posts and comments.

pub mod actions;
pub mod app;
pub mod config;
pub mod factories;
pub mod forms;
pub mod jobs;
pub mod mail;
pub mod notifications;
pub mod ops;
pub mod parts;
pub mod policies;
pub mod records;
pub mod schedule;
pub mod seed;
pub mod views;

pub use seed::seed;
