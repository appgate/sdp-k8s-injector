use std::{
    collections::{BTreeMap, HashMap},
    error::Error,
};

use k8s_openapi::api::core::v1::Pod;
use kube::{Resource, ResourceExt};

use crate::service::ServiceUser;

pub trait Named {
    fn name(&self) -> String;
}

pub trait Namespaced {
    fn namespace(&self) -> Option<String>;
}

/// Trait that defines entities that are candidates to be services
/// Basically a service candidate needs to be able to define :
///  - namespace
///  - name
/// and the combination of both needs to be unique
pub trait Candidate {
    fn is_candidate(&self) -> bool;
}

pub trait Annotated {
    fn annotations(&self) -> Option<&BTreeMap<String, String>>;

    fn annotation(&self, annotation: &str) -> Option<&String> {
        self.annotations().and_then(|m| m.get(annotation))
    }
}

pub trait Service: Named + Namespaced + Sized {
    fn service_name(&self) -> Result<String, Box<dyn Error>> {
        let namespace =
            Namespaced::namespace(self).ok_or_else(|| "Namespace not found in resource")?;
        Ok(format!("{}-{}", namespace, Named::name(self)))
    }
    fn service_id(&self) -> Result<String, Box<dyn Error>> {
        let namespace =
            Namespaced::namespace(self).ok_or_else(|| "Namespace not found in resource")?;
        Ok(format!("{}_{}", namespace, Named::name(self)))
    }
}

pub trait Labelled: Service {
    fn labels(&self) -> Result<HashMap<String, String>, Box<dyn Error>>;
}

pub trait Validated {
    fn validate<R: ObjectRequest<Pod>>(&self, request: R) -> Result<(), String>;
}

pub trait ObjectRequest<O: Resource> {
    fn object(&self) -> Option<&O>;
}

pub trait HasCredentials {
    fn credentials<'a>(&'a self) -> &'a ServiceUser;
}
