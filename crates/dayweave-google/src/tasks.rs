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
        let url = self.endpoint(&["tasks", "v1", "lists", task_list_id, "tasks"])?;
        let request = self.request(Method::POST, url).await?;
        self.json(Self::body(request, task)).await
    }

    /// Updates a task conditionally using the last-seen `ETag`.
    ///
    /// # Errors
    ///
    /// Returns [`GoogleError::PreconditionFailed`] for a stale record and typed
    /// transport/provider errors otherwise.
    pub async fn update_task(
        &self,
        task_list_id: &str,
        task: &GoogleTask,
    ) -> Result<GoogleTask, GoogleError> {
        let url = self.endpoint(&["tasks", "v1", "lists", task_list_id, "tasks", &task.id])?;
        let mut request = self.request(Method::PUT, url).await?;
        if let Some(etag) = &task.etag {
            request = request.header(reqwest::header::IF_MATCH, etag);
        }
        self.json(Self::body(request, task)).await
    }

    /// Deletes a task after the service layer has moved its canonical `DayWeave`
    /// record to recoverable trash.
    ///
    /// # Errors
    ///
    /// Returns stale-write, transport, authorization, or API errors.
    pub async fn delete_task(
        &self,
        task_list_id: &str,
        task_id: &str,
        etag: Option<&str>,
    ) -> Result<(), GoogleError> {
        let url = self.endpoint(&["tasks", "v1", "lists", task_list_id, "tasks", task_id])?;
        let mut request = self.request(Method::DELETE, url).await?;
        if let Some(etag) = etag {
            request = request.header(reqwest::header::IF_MATCH, etag);
        }
        self.empty(request).await
    }
}
