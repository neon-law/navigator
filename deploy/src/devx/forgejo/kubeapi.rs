//! The runner controller's Kubernetes client: the in-cluster API with the
//! pod's own service-account token, limited to the one runner namespace.
//!
//! The CLI image is a static binary with no `kubectl`, so the controller
//! speaks the REST API directly. Its Role (`k8s/forgejo/runner-controller/`)
//! grants Jobs, Secrets, `ServiceAccounts`, `NetworkPolicies` and pod logs in the
//! runner namespace only; nothing here can reach another namespace.

use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use reqwest::{Method, StatusCode};
use serde_json::Value;

use super::runner::{Cluster, RUNNER_JOB_LABEL};

const SERVICE_ACCOUNT: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

pub(in crate::devx) struct InCluster {
    client: reqwest::Client,
    base: String,
    token: String,
    namespace: String,
    runtime: tokio::runtime::Runtime,
}

impl InCluster {
    /// Read the pod's service-account token and cluster CA.
    pub(in crate::devx) fn from_pod(namespace: &str) -> Result<Self> {
        let host = std::env::var("KUBERNETES_SERVICE_HOST")
            .context("KUBERNETES_SERVICE_HOST is unset; run in-cluster or pass --context")?;
        let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".into());
        let token = std::fs::read_to_string(format!("{SERVICE_ACCOUNT}/token"))
            .context("read the service-account token")?;
        let ca =
            std::fs::read(format!("{SERVICE_ACCOUNT}/ca.crt")).context("read the cluster CA")?;
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host
        };
        Self::new(
            &format!("https://{host}:{port}"),
            token.trim().to_owned(),
            namespace,
            Some(reqwest::Certificate::from_pem(&ca)?),
        )
    }

    fn new(
        base: &str,
        token: String,
        namespace: &str,
        ca: Option<reqwest::Certificate>,
    ) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none());
        if let Some(ca) = ca {
            builder = builder.add_root_certificate(ca);
        }
        Ok(Self {
            client: builder.build()?,
            base: base.trim_end_matches('/').to_owned(),
            token,
            namespace: namespace.to_owned(),
            runtime: tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()?,
        })
    }

    fn namespaced(&self, group: &str, resource: &str) -> String {
        format!(
            "{}/{group}/namespaces/{}/{resource}",
            self.base, self.namespace
        )
    }

    fn send(
        &self,
        method: Method,
        url: &str,
        body: Option<(&str, Vec<u8>)>,
    ) -> Result<(StatusCode, String)> {
        self.runtime.block_on(async {
            let mut request = self.client.request(method, url).bearer_auth(&self.token);
            if let Some((content_type, bytes)) = body {
                request = request
                    .header(reqwest::header::CONTENT_TYPE, content_type)
                    .body(bytes);
            }
            let response = request.send().await.context("reach the Kubernetes API")?;
            let status = response.status();
            Ok((status, response.text().await.unwrap_or_default()))
        })
    }

    fn get_json(&self, url: &str) -> Result<Option<Value>> {
        let (status, body) = self.send(Method::GET, url, None)?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            status.is_success(),
            "Kubernetes API read refused ({status})"
        );
        Ok(Some(serde_json::from_str(&body)?))
    }

    /// The collection URL for one object this controller may create.
    fn collection(&self, object: &Value) -> Result<String> {
        Ok(match object["kind"].as_str().unwrap_or_default() {
            "Job" => self.namespaced("apis/batch/v1", "jobs"),
            "Secret" => self.namespaced("api/v1", "secrets"),
            "ServiceAccount" => self.namespaced("api/v1", "serviceaccounts"),
            "NetworkPolicy" => self.namespaced("apis/networking.k8s.io/v1", "networkpolicies"),
            other => bail!("the runner controller does not create {other}"),
        })
    }
}

impl Cluster for InCluster {
    fn namespace(&self) -> &str {
        &self.namespace
    }

    fn create(&self, object: &Value) -> Result<()> {
        ensure!(
            object["metadata"]["namespace"].as_str() == Some(self.namespace.as_str()),
            "a runner object must live in the runner namespace"
        );
        let url = self.collection(object)?;
        let (status, _) = self.send(
            Method::POST,
            &url,
            Some(("application/json", serde_json::to_vec(object)?)),
        )?;
        ensure!(
            status == StatusCode::CREATED,
            "Kubernetes resource creation failed ({status}); inspect the controller Role"
        );
        Ok(())
    }

    fn job(&self, name: &str) -> Result<Option<Value>> {
        self.get_json(&format!(
            "{}/{name}",
            self.namespaced("apis/batch/v1", "jobs")
        ))
    }

    fn unsuspend(&self, name: &str) -> Result<()> {
        let (status, _) = self.send(
            Method::PATCH,
            &format!("{}/{name}", self.namespaced("apis/batch/v1", "jobs")),
            Some((
                "application/merge-patch+json",
                br#"{"spec":{"suspend":false}}"#.to_vec(),
            )),
        )?;
        ensure!(
            status.is_success(),
            "starting the runner Job was refused ({status})"
        );
        Ok(())
    }

    fn delete_job(&self, name: &str) -> Result<()> {
        let (status, _) = self.send(
            Method::DELETE,
            &format!("{}/{name}", self.namespaced("apis/batch/v1", "jobs")),
            Some((
                "application/json",
                br#"{"propagationPolicy":"Foreground"}"#.to_vec(),
            )),
        )?;
        ensure!(
            status.is_success() || status == StatusCode::NOT_FOUND,
            "deleting the runner Job was refused ({status})"
        );
        Ok(())
    }

    fn runner_namespace(&self) -> Result<Value> {
        self.get_json(&format!(
            "{}/api/v1/namespaces/{}",
            self.base, self.namespace
        ))?
        .context("the runner namespace does not exist")
    }

    fn network_policies(&self) -> Result<Value> {
        self.get_json(&self.namespaced("apis/networking.k8s.io/v1", "networkpolicies"))?
            .context("NetworkPolicy inventory is unavailable")
    }

    fn runner_jobs(&self) -> Result<Vec<Value>> {
        let listed = self
            .get_json(&format!(
                "{}?labelSelector={RUNNER_JOB_LABEL}%3Dtrue",
                self.namespaced("apis/batch/v1", "jobs")
            ))?
            .context("runner Job inventory is unavailable")?;
        Ok(listed["items"].as_array().cloned().unwrap_or_default())
    }

    fn job_log(&self, name: &str) -> Result<String> {
        let pods = self
            .get_json(&format!(
                "{}?labelSelector=job-name%3D{name}",
                self.namespaced("api/v1", "pods")
            ))?
            .context("runner pod inventory is unavailable")?;
        let Some(pod) = pods["items"]
            .as_array()
            .and_then(|items| items.first())
            .and_then(|pod| pod["metadata"]["name"].as_str())
        else {
            return Ok(String::new());
        };
        let (status, body) = self.send(
            Method::GET,
            &format!("{}/{pod}/log", self.namespaced("api/v1", "pods")),
            None,
        )?;
        ensure!(
            status.is_success(),
            "runner pod log read refused ({status})"
        );
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cluster(server: &MockServer) -> InCluster {
        InCluster::new(&server.uri(), "sa-token".into(), "runners", None).unwrap()
    }

    #[test]
    fn only_runner_objects_in_the_runner_namespace_are_created() {
        // Every refusal happens before a request, so no server is needed.
        let api = InCluster::new("http://127.0.0.1:9", "sa-token".into(), "runners", None).unwrap();
        for object in [
            json!({"kind": "Job", "metadata": {"namespace": "navigator"}}),
            json!({"kind": "Role", "metadata": {"namespace": "runners"}}),
            json!({"kind": "Pod", "metadata": {"namespace": "runners"}}),
        ] {
            assert!(api.create(&object).is_err(), "{object}");
        }
    }

    #[test]
    fn jobs_are_created_started_and_listed_through_the_namespaced_api() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/apis/batch/v1/namespaces/runners/jobs"))
                .and(header("authorization", "Bearer sa-token"))
                .respond_with(ResponseTemplate::new(201))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("PATCH"))
                .and(path("/apis/batch/v1/namespaces/runners/jobs/forgejo-job-x"))
                .and(header("content-type", "application/merge-patch+json"))
                .and(body_json(json!({"spec": {"suspend": false}})))
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/apis/batch/v1/namespaces/runners/jobs"))
                .and(query_param(
                    "labelSelector",
                    "navigator.neonlaw.org/forgejo-runner=true",
                ))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"items": [{"metadata": {"name": "forgejo-job-x"}}]})),
                )
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/apis/batch/v1/namespaces/runners/jobs/missing"))
                .respond_with(ResponseTemplate::new(404))
                .mount(&server)
                .await;
            server
        });
        let api = cluster(&server);
        api.create(&json!({"kind": "Job", "metadata": {"namespace": "runners"}}))
            .unwrap();
        api.unsuspend("forgejo-job-x").unwrap();
        assert_eq!(api.runner_jobs().unwrap().len(), 1);
        assert!(api.job("missing").unwrap().is_none());
        drop(runtime);
    }

    #[test]
    fn identities_policies_logs_and_deletes_stay_in_the_runner_namespace() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(async {
            let server = MockServer::start().await;
            for collection in [
                "/api/v1/namespaces/runners/secrets",
                "/api/v1/namespaces/runners/serviceaccounts",
                "/apis/networking.k8s.io/v1/namespaces/runners/networkpolicies",
            ] {
                Mock::given(method("POST"))
                    .and(path(collection))
                    .respond_with(ResponseTemplate::new(201))
                    .expect(1)
                    .mount(&server)
                    .await;
            }
            Mock::given(method("GET"))
                .and(path("/api/v1/namespaces/runners"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"metadata": {"name": "runners"}})),
                )
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path(
                    "/apis/networking.k8s.io/v1/namespaces/runners/networkpolicies",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/v1/namespaces/runners/pods"))
                .and(query_param("labelSelector", "job-name=forgejo-job-x"))
                .respond_with(ResponseTemplate::new(200).set_body_json(
                    json!({"items": [{"metadata": {"name": "forgejo-job-x-abcde"}}]}),
                ))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/v1/namespaces/runners/pods"))
                .and(query_param("labelSelector", "job-name=forgejo-job-y"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": []})))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path(
                    "/api/v1/namespaces/runners/pods/forgejo-job-x-abcde/log",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_string("step one\n"))
                .mount(&server)
                .await;
            Mock::given(method("DELETE"))
                .and(path("/apis/batch/v1/namespaces/runners/jobs/forgejo-job-x"))
                .respond_with(ResponseTemplate::new(404))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("DELETE"))
                .and(path("/apis/batch/v1/namespaces/runners/jobs/forgejo-job-y"))
                .respond_with(ResponseTemplate::new(403))
                .mount(&server)
                .await;
            server
        });
        let api = cluster(&server);
        assert_eq!(api.namespace(), "runners");
        for kind in ["Secret", "ServiceAccount", "NetworkPolicy"] {
            api.create(&json!({"kind": kind, "metadata": {"namespace": "runners"}}))
                .unwrap();
        }
        assert_eq!(
            api.runner_namespace().unwrap()["metadata"]["name"],
            "runners"
        );
        assert_eq!(api.network_policies().unwrap(), json!({"items": []}));
        assert_eq!(api.job_log("forgejo-job-x").unwrap(), "step one\n");
        // A Job whose pod is already gone has an empty log.
        assert_eq!(api.job_log("forgejo-job-y").unwrap(), "");
        api.delete_job("forgejo-job-x").unwrap();
        assert!(api.delete_job("forgejo-job-y").is_err());
        drop(runtime);
    }
}
