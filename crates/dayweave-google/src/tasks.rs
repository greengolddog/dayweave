use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::{GoogleClient, GoogleError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskListPage {
    #[serde(default)]
    pub items: Vec<GoogleTaskList>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleTaskList {
    pub id: String,
    pub etag: Option<String>,
    pub title: String,
    pub updated: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskPage {
    #[serde(default)]
    pub items: Vec<GoogleTask>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoogleTask {
    pub id: String,
    pub etag: Option<String>,
    /// Deleted-task tombstones may omit user content.
    #[serde(default)]
    pub title: String,
    pub notes: Option<String>,
    pub status: Option<String>,
    pub due: Option<String>,
    pub completed: Option<String>,
    pub updated: Option<String>,
    pub parent: Option<String>,
    pub position: Option<String>,
    pub links: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GoogleTaskWrite<'a> {
    title: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    due: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed: Option<&'a str>,
}

impl<'a> From<&'a GoogleTask> for GoogleTaskWrite<'a> {
    fn from(task: &'a GoogleTask) -> Self {
        Self {
            title: &task.title,
            notes: task.notes.as_deref(),
            status: task.status.as_deref(),
            due: task.due.as_deref(),
            completed: task.completed.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskInsertOptions {
    /// Creates a subtask under this remote task ID.
    pub parent: Option<String>,
    /// Places the task after this sibling remote task ID.
    pub previous: Option<String>,
}

impl GoogleClient {
    /// Lists Google Tasks lists visible to the account.
    ///
    /// # Errors
    ///
    /// Returns typed transport, authorization, rate-limit, or provider errors.
    pub async fn list_task_lists(
        &self,
        page_token: Option<&str>,
    ) -> Result<TaskListPage, GoogleError> {
        let url = self.endpoint(&["tasks", "v1", "users", "@me", "lists"])?;
        let query = page_token
            .map(|value| vec![("pageToken", value)])
            .unwrap_or_default();
        let request = self.request(Method::GET, url).await?.query(&query);
        self.json(request).await
    }

    /// Lists tasks including completed, hidden, and deleted records so local
    /// state can converge instead of silently resurrecting tombstones.
    ///
    /// # Errors
    ///
    /// Returns typed transport, authorization, rate-limit, or provider errors.
    pub async fn list_tasks(
        &self,
        task_list_id: &str,
        page_token: Option<&str>,
        updated_min: Option<&str>,
    ) -> Result<TaskPage, GoogleError> {
        let url = self.endpoint(&["tasks", "v1", "lists", task_list_id, "tasks"])?;
        let mut query = vec![
            ("showCompleted", "true"),
            ("showDeleted", "true"),
            ("showHidden", "true"),
            ("maxResults", "100"),
        ];
        if let Some(value) = page_token {
            query.push(("pageToken", value));
        }
        if let Some(value) = updated_min {
            query.push(("updatedMin", value));
        }
        let request = self.request(Method::GET, url).await?.query(&query);
        self.json(request).await
    }

    /// Inserts a task into a selected Google Tasks list.
    ///
    /// # Errors
    ///
    /// Returns typed transport, authorization, rate-limit, or provider errors.
    pub async fn insert_task(
        &self,
        task_list_id: &str,
        task: &GoogleTask,
    ) -> Result<GoogleTask, GoogleError> {
        self.insert_task_at(task_list_id, task, &TaskInsertOptions::default())
            .await
    }

    /// Inserts and positions a top-level task or subtask.
    ///
    /// # Errors
    ///
    /// Returns typed transport, authorization, rate-limit, or provider errors.
    pub async fn prepare_insert_task_at(
        &self,
        task_list_id: &str,
        task: &GoogleTask,
        options: &TaskInsertOptions,
    ) -> Result<crate::PreparedGoogleRequest, GoogleError> {
        let url = self.endpoint(&["tasks", "v1", "lists", task_list_id, "tasks"])?;
        let mut query = Vec::new();
        if let Some(parent) = &options.parent {
            query.push(("parent", parent));
        }
        if let Some(previous) = &options.previous {
            query.push(("previous", previous));
        }
        let request = self.request(Method::POST, url).await?.query(&query);
        self.prepare(Self::body(request, &GoogleTaskWrite::from(task)))
    }

    /// Prepares a top-level task insert without contacting Google.
    ///
    /// # Errors
    ///
    /// Returns typed request-construction or authorization errors.
    pub async fn prepare_insert_task(
        &self,
        task_list_id: &str,
        task: &GoogleTask,
    ) -> Result<crate::PreparedGoogleRequest, GoogleError> {
        self.prepare_insert_task_at(task_list_id, task, &TaskInsertOptions::default())
            .await
    }

    /// Inserts and positions a top-level task or subtask.
    ///
    /// # Errors
    ///
    /// Returns typed transport, authorization, rate-limit, or provider errors.
    pub async fn insert_task_at(
        &self,
        task_list_id: &str,
        task: &GoogleTask,
        options: &TaskInsertOptions,
    ) -> Result<GoogleTask, GoogleError> {
        self.prepare_insert_task_at(task_list_id, task, options)
            .await?
            .send_json(None)
            .await
    }

    /// Updates a task conditionally using the last-seen `ETag`.
    /// # Errors
    ///
    /// Returns [`GoogleError::PreconditionFailed`] for a stale record and typed
    /// transport/provider errors otherwise.
    pub async fn prepare_update_task(
        &self,
        task_list_id: &str,
        task: &GoogleTask,
    ) -> Result<crate::PreparedGoogleRequest, GoogleError> {
        let etag = task
            .etag
            .as_deref()
            .filter(|etag| !etag.trim().is_empty())
            .ok_or(GoogleError::ConditionalWriteRequired)?;
        let url = self.endpoint(&["tasks", "v1", "lists", task_list_id, "tasks", &task.id])?;
        let mut request = self.request(Method::PUT, url).await?;
        request = request.header(reqwest::header::IF_MATCH, etag);
        self.prepare(Self::body(request, &GoogleTaskWrite::from(task)))
    }

    /// Updates a task conditionally using the last-seen `ETag`.
    ///
    /// # Errors
    ///
    /// Returns stale-write, transport, authorization, or provider errors.
    pub async fn update_task(
        &self,
        task_list_id: &str,
        task: &GoogleTask,
    ) -> Result<GoogleTask, GoogleError> {
        self.prepare_update_task(task_list_id, task)
            .await?
            .send_json(None)
            .await
    }

    /// Deletes a task after the service layer has moved its canonical `DayWeave`
    /// record to recoverable trash.
    ///
    /// # Errors
    ///
    /// Returns stale-write, transport, authorization, or API errors.
    pub async fn prepare_delete_task(
        &self,
        task_list_id: &str,
        task_id: &str,
        etag: &str,
    ) -> Result<crate::PreparedGoogleRequest, GoogleError> {
        if etag.trim().is_empty() {
            return Err(GoogleError::ConditionalWriteRequired);
        }
        let url = self.endpoint(&["tasks", "v1", "lists", task_list_id, "tasks", task_id])?;
        let mut request = self.request(Method::DELETE, url).await?;
        request = request.header(reqwest::header::IF_MATCH, etag);
        self.prepare(request)
    }

    /// Deletes a task conditionally using its last-seen `ETag`.
    ///
    /// # Errors
    ///
    /// Returns stale-write, transport, authorization, or provider errors.
    pub async fn delete_task(
        &self,
        task_list_id: &str,
        task_id: &str,
        etag: &str,
    ) -> Result<(), GoogleError> {
        self.prepare_delete_task(task_list_id, task_id, etag)
            .await?
            .send_empty(None)
            .await
    }
}
