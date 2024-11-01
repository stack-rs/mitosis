//! `SeaORM` Entity. Generated by sea-orm-codegen 0.12.15

pub mod prelude;

pub mod active_tasks;
pub mod archived_tasks;
pub mod artifacts;
pub mod attachments;
pub mod content;
pub mod group_worker;
pub mod groups;
pub mod role;
pub mod state;
pub mod user_group;
pub mod users;
pub mod workers;

pub enum StoredTaskModel {
    Active(active_tasks::Model),
    Archived(archived_tasks::Model),
}