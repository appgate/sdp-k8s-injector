use kube::core::admission::AdmissionResponse;
use sdp_common::errors::SDPServiceError;

#[derive(Debug)]
pub enum SDPPatchError {
    WithResponse(Box<AdmissionResponse>, SDPServiceError),
    WithoutResponse(SDPServiceError),
}

impl SDPPatchError {
    pub fn from_admission_response(
        response: Box<AdmissionResponse>,
    ) -> impl FnOnce(SDPServiceError) -> Self {
        move |e: SDPServiceError| SDPPatchError::WithResponse(Box::clone(&response), e)
    }
}

impl From<SDPServiceError> for SDPPatchError {
    fn from(e: SDPServiceError) -> Self {
        SDPPatchError::WithoutResponse(e)
    }
}
