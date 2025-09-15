use std::{borrow::Cow, path::PathBuf};

use clap_repl::{
    reedline::{self, FileBackedHistory},
    ClapEditor,
};
use humansize::{format_size, DECIMAL};

use crate::{
    config::client::ClientInteractiveShell,
    entity::role::GroupWorkerRole,
    schema::{
        AdminChangePasswordReq, CreateUserReq, GroupQueryInfo, ParsedTaskQueryInfo, TaskQueryInfo,
        UserChangePasswordReq, WorkerQueryInfo,
    },
    service::auth::{get_and_prompt_password, get_and_prompt_username},
};
static DEFAULT_LEFT_PROMPT: &str = "[mito::client]";
static DEFAULT_INDICATOR: &str = "> ";
static DEFAULT_MULTILINE_INDICATOR: &str = "::: ";

pub struct MitoPrompt;

impl reedline::Prompt for MitoPrompt {
    fn render_prompt_left(&self) -> std::borrow::Cow<'_, str> {
        Cow::Borrowed(DEFAULT_LEFT_PROMPT)
    }

    fn render_prompt_right(&self) -> std::borrow::Cow<'_, str> {
        Cow::Borrowed("")
    }

    fn render_prompt_indicator(
        &self,
        _prompt_mode: reedline::PromptEditMode,
    ) -> std::borrow::Cow<'_, str> {
        Cow::Borrowed(DEFAULT_INDICATOR)
    }

    fn render_prompt_multiline_indicator(&self) -> std::borrow::Cow<'_, str> {
        Cow::Borrowed(DEFAULT_MULTILINE_INDICATOR)
    }

    fn render_prompt_history_search_indicator(
        &self,
        history_search: reedline::PromptHistorySearch,
    ) -> std::borrow::Cow<'_, str> {
        let prefix = match history_search.status {
            reedline::PromptHistorySearchStatus::Passing => "",
            reedline::PromptHistorySearchStatus::Failing => "failing ",
        };
        // NOTE: magic strings, given there is logic on how these compose I am not sure if it
        // is worth extracting in to static constant
        Cow::Owned(format!(
            "({}reverse-search: {}) ",
            prefix, history_search.term
        ))
    }
}

pub(crate) fn get_interactive_shell(
    cache_file: Option<PathBuf>,
) -> ClapEditor<ClientInteractiveShell> {
    let prompt = MitoPrompt;

    ClapEditor::<ClientInteractiveShell>::builder()
        .with_prompt(Box::new(prompt))
        .with_editor_hook(|reed| {
            // Do custom things with `Reedline` instance here
            if let Some(history_file) = cache_file {
                reed.with_history(Box::new(
                    FileBackedHistory::with_file(1000, history_file).unwrap(),
                ))
            } else {
                reed
            }
        })
        .build()
}

pub(crate) fn output_parsed_task_info(info: &ParsedTaskQueryInfo) {
    tracing::info!("Task UUID: {}", info.uuid);
    tracing::info!("State: {}", info.state);
    tracing::info!(
        "Created by user {} as the #{} task in Group {}",
        info.creator_username,
        info.task_id,
        info.group_name
    );
    tracing::info!("Tags: {:?}", info.tags);
    tracing::info!("Labels: {:?}", info.labels);
    let timeout = std::time::Duration::from_secs(info.timeout as u64);
    tracing::info!("Timeout {:?} and Priority {}", timeout, info.priority);
    tracing::info!(
        "Created at {} and Updated at {}",
        info.created_at,
        info.updated_at
    );
    tracing::info!("Task Spec: {:?}", info.spec);
    if let Some(result) = &info.result {
        tracing::info!("Task Result: {:?}", result);
    } else {
        tracing::info!("Task Result: None");
    }
    if let Some(upstream_task_uuid) = info.upstream_task_uuid {
        tracing::info!("Upstream Task UUID: {}", upstream_task_uuid);
    }
    if let Some(downstream_task_uuid) = info.downstream_task_uuid {
        tracing::info!("Downstream Task UUID: {:?}", downstream_task_uuid);
    }
}

pub(crate) fn output_task_info(info: &TaskQueryInfo) {
    tracing::info!("Task UUID: {}", info.uuid);
    tracing::info!("State: {}", info.state);
    tracing::info!(
        "Created by user {} as the #{} task in Group {}",
        info.creator_username,
        info.task_id,
        info.group_name
    );
    tracing::info!("Tags: {:?}", info.tags);
    tracing::info!("Labels: {:?}", info.labels);
    let timeout = std::time::Duration::from_secs(info.timeout as u64);
    tracing::info!("Timeout {:?} and Priority {}", timeout, info.priority);
    tracing::info!(
        "Created at {} and Updated at {}",
        info.created_at,
        info.updated_at
    );
    tracing::info!("Task Spec: {}", info.spec);
    if let Some(result) = &info.result {
        tracing::info!("Task Result: {}", result);
    } else {
        tracing::info!("Task Result: None");
    }
    if let Some(upstream_task_uuid) = info.upstream_task_uuid {
        tracing::info!("Upstream Task UUID: {}", upstream_task_uuid);
    }
    if let Some(downstream_task_uuid) = info.downstream_task_uuid {
        tracing::info!("Downstream Task UUID: {:?}", downstream_task_uuid);
    }
}

pub(crate) fn output_worker_list_info<T: std::fmt::Display>(
    info: &WorkerQueryInfo,
    group_name: &T,
) {
    tracing::info!("Worker UUID: {}", info.worker_id);
    tracing::info!("Tags: {:?}", info.tags);
    tracing::info!("State: {}", info.state);
    tracing::info!(
        "Created by user {} for group {}",
        info.creator_username,
        group_name
    );
    tracing::info!(
        "Created at {} and Updated at {}",
        info.created_at,
        info.updated_at
    );
    tracing::info!("Last Heartbeat: {}", info.last_heartbeat);
    if let Some(task) = info.assigned_task_id {
        tracing::info!("Assigned Task: {}", task);
    } else {
        tracing::info!("Assigned Task: None");
    }
}

pub(crate) fn output_worker_info(
    info: &WorkerQueryInfo,
    groups: &std::collections::HashMap<String, GroupWorkerRole>,
) {
    tracing::info!("Worker UUID: {}", info.worker_id);
    tracing::info!("Tags: {:?}", info.tags);
    tracing::info!("State: {}", info.state);
    tracing::info!("Accessible Groups: {:?}", groups);
    tracing::info!("Created by user {} ", info.creator_username,);
    tracing::info!(
        "Created at {} and Updated at {}",
        info.created_at,
        info.updated_at
    );
    tracing::info!("Last Heartbeat: {}", info.last_heartbeat);
    if let Some(task) = info.assigned_task_id {
        tracing::info!("Assigned Task: {}", task);
    } else {
        tracing::info!("Assigned Task: None");
    }
}

pub(crate) fn output_group_info(info: &GroupQueryInfo) {
    tracing::info!("Group Name: {}", info.group_name);
    tracing::info!("Created by user {}", info.creator_username);
    tracing::info!(
        "Created at {} and Updated at {}",
        info.created_at,
        info.updated_at
    );
    tracing::info!("State: {}", info.state);
    tracing::info!("Contains {} tasks", info.task_count);
    tracing::info!(
        "Storage(used/total): {} / {}",
        format_size((info.storage_used).max(0) as u64, DECIMAL),
        format_size((info.storage_quota).max(0) as u64, DECIMAL)
    );
    tracing::info!("Can access {} workers", info.worker_count);
    if let Some(ref users) = info.users_in_group {
        tracing::info!("Users in the group:");
        for (user, role) in users {
            tracing::info!(" > {} = {}", user, role);
        }
    }
}

pub(crate) fn fill_admin_create_user(
    username: Option<String>,
    password: Option<String>,
    admin: bool,
) -> crate::error::Result<CreateUserReq> {
    match (username, password) {
        (Some(username), Some(password)) => Ok(CreateUserReq {
            username,
            md5_password: md5::compute(password.as_bytes()).0,
            admin,
        }),
        (username, password) => {
            let username = get_and_prompt_username(username, "Username of new user")?;
            let md5_password = get_and_prompt_password(password, "Password of new user")?;
            Ok(CreateUserReq {
                username,
                md5_password,
                admin,
            })
        }
    }
}

pub(crate) fn fill_admin_change_password(
    username: Option<String>,
    password: Option<String>,
) -> crate::error::Result<(String, AdminChangePasswordReq)> {
    match (username, password) {
        (Some(username), Some(password)) => Ok((
            username,
            AdminChangePasswordReq {
                new_md5_password: md5::compute(password.as_bytes()).0,
            },
        )),
        (username, password) => {
            let username = get_and_prompt_username(username, "Username")?;
            let new_md5_password = get_and_prompt_password(password, "New Password")?;
            Ok((username, AdminChangePasswordReq { new_md5_password }))
        }
    }
}

pub(crate) fn fill_user_change_password(
    username: Option<String>,
    old_password: Option<String>,
    new_password: Option<String>,
) -> crate::error::Result<(String, UserChangePasswordReq)> {
    match (username, old_password, new_password) {
        (Some(username), Some(old_password), Some(new_password)) => Ok((
            username,
            UserChangePasswordReq {
                old_md5_password: md5::compute(old_password.as_bytes()).0,
                new_md5_password: md5::compute(new_password.as_bytes()).0,
            },
        )),
        (username, old_password, new_password) => {
            let username = get_and_prompt_username(username, "Username")?;
            let old_md5_password = get_and_prompt_password(old_password, "Old Password")?;
            let new_md5_password = get_and_prompt_password(new_password, "New Password")?;
            Ok((
                username,
                UserChangePasswordReq {
                    old_md5_password,
                    new_md5_password,
                },
            ))
        }
    }
}
