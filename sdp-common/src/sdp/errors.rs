use crate::sdp::system::ResponseData;
use reqwest::{Error as RError, Response, StatusCode};
use serde::de::DeserializeOwned;
use std::fmt::Display;

pub struct SDPClientError {
    pub request_error: Option<RError>,
    pub status_code: Option<reqwest::StatusCode>,
    pub error_body: Option<String>,
}

impl Display for SDPClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.request_error.is_some() {
            write!(
                f,
                "SDPClient error: {:?}",
                self.request_error.as_ref().unwrap()
            )
        } else {
            write!(
                f,
                "SDPClient error [{:?}]: {:?}",
                self.status_code, self.error_body
            )
        }
    }
}

impl From<RError> for SDPClientError {
    fn from(error: RError) -> Self {
        SDPClientError {
            request_error: Some(error),
            status_code: None,
            error_body: None,
        }
    }
}

pub async fn error_for_status<D: DeserializeOwned>(
    response: Response,
) -> Result<ResponseData<D>, SDPClientError> {
    if response.status().is_client_error() || response.status().is_server_error() {
        let status = response.status();
        let body = response.text().await?;
        Err(SDPClientError {
            request_error: None,
            status_code: Some(status),
            error_body: Some(body),
        })
    } else if response.status() == StatusCode::NO_CONTENT {
        Ok(ResponseData::NoContent)
    } else {
        Ok(ResponseData::Entity(response.json::<D>().await?))
    }
}
