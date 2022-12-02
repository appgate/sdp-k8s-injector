use sdp_common::errors::SDPServiceError;

const DEVICE_ID_SERVICE: &str = "DeviceIdManager";

pub struct DeviceIdServiceError(SDPServiceError);

impl From<&str> for DeviceIdServiceError {
    fn from(error: &str) -> Self {
        DeviceIdServiceError(
            SDPServiceError::from_string(error.to_string())
                .with_service(DEVICE_ID_SERVICE.to_string()),
        )
    }
}
