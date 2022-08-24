use std::fmt::Display;

pub struct DeviceIdServiceError {
    who: Option<String>,
    error: String,
}

impl DeviceIdServiceError {}

impl Display for DeviceIdServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.who.is_some() {
            write!(
                f,
                "DeviceIdService [{}] error: {}",
                self.who.as_ref().unwrap(),
                self.error
            )
        } else {
            write!(f, "DeviceIdService error: {}", self.error)
        }
    }
}
