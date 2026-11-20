//! Private scratch Pod; neither a Service nor a live database is reconfigured.
use super::super::super::{run, Tier, OWNER_LABEL};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub(super) struct Scratch {
    pub name: String,
    pub pod_uid: Option<String>,
    pub config_uid: Option<String>,
    pub creation_inflight: bool,
}

impl Scratch {
    pub fn new(tier: &Tier) -> Self {
        Self {
            name: format!("physical-{}", tier.operation),
            pod_uid: None,
            config_uid: None,
            creation_inflight: false,
        }
    }

    pub fn verify(&self, tier: &Tier) -> Value {
        tier.verify();
        let pod = tier.json(&["get", "pod", &self.name, "-o", "json"]);
        assert_eq!(pod["metadata"]["uid"].as_str(), self.pod_uid.as_deref());
        assert_eq!(pod["metadata"]["labels"][OWNER_LABEL], tier.operation);
        assert_eq!(
            pod["spec"]["containers"][0]["image"],
            tier.template["spec"]["template"]["spec"]["containers"][0]["image"]
        );
        assert_eq!(pod["spec"]["securityContext"]["runAsUser"], 999);
        assert_eq!(pod["spec"]["automountServiceAccountToken"], false);
        assert!(pod["spec"]["volumes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v.get("hostPath").is_none()));
        let env = pod["spec"]["containers"][0]["env"].as_array().unwrap();
        assert_eq!(env.len(), 7);
        assert!(env
            .iter()
            .all(|value| value["name"].as_str().unwrap().starts_with("PG")));
        let expected = self.pod(tier);
        for key in [
            "containers",
            "volumes",
            "securityContext",
            "automountServiceAccountToken",
        ] {
            super::super::super::assert_canonical_fields(&pod["spec"][key], &expected["spec"][key]);
        }
        for key in ["hostPID", "hostIPC", "hostNetwork"] {
            assert!(!pod["spec"][key].as_bool().unwrap_or(false));
        }
        assert!(pod["spec"].get("initContainers").is_none());
        let config = tier.json(&["get", "configmap", &self.name, "-o", "json"]);
        assert_eq!(
            config["metadata"]["uid"].as_str(),
            self.config_uid.as_deref()
        );
        assert_eq!(config["metadata"]["labels"][OWNER_LABEL], tier.operation);
        assert_eq!(config["immutable"], true);
        assert_eq!(config["data"], self.configuration(tier)["data"]);
        let digest = pod["spec"]["containers"][0]["image"]
            .as_str()
            .unwrap()
            .split_once('@')
            .unwrap()
            .1;
        assert!(pod["status"]["containerStatuses"][0]["imageID"]
            .as_str()
            .unwrap()
            .ends_with(digest));
        pod
    }

    pub fn exec(&self, tier: &Tier, args: &[&str]) -> Vec<u8> {
        self.verify(tier);
        let mut cmd = tier.kubectl(&["exec", &self.name, "-c", "recovery", "--"]);
        cmd.args(args);
        run(&tier.health, &mut cmd, None)
    }

    pub fn create(&mut self, tier: &Tier) {
        tier.verify();
        let source = tier.json(&["get", "pods", "-l", "app=navigator-postgres", "-o", "json"]);
        let image = &tier.template["spec"]["template"]["spec"]["containers"][0]["image"];
        assert_eq!(source["items"][0]["spec"]["containers"][0]["image"], *image);
        let endpoints = tier.json(&[
            "get",
            "endpointslices",
            "-l",
            "kubernetes.io/service-name=navigator-postgres",
            "-o",
            "json",
        ]);
        let endpoints: Vec<_> = endpoints["items"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|slice| slice["endpoints"].as_array().unwrap())
            .collect();
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0]["targetRef"]["uid"], tier.pod_uid);
        assert_eq!(endpoints[0]["conditions"]["ready"], true);
        // Refuse missing backup identity, never create credentials or grant replication here.
        let secret = tier.json(&["get", "secret", "navigator-postgres-backup", "-o", "json"]);
        assert!(secret["data"]["password"].is_string());
        let config = self.configuration(tier);
        self.creation_inflight = true;
        let observed: Value = serde_json::from_slice(&run(
            &tier.health,
            &mut tier.kubectl(&["create", "-f", "-", "-o", "json"]),
            Some(&serde_json::to_vec(&config).unwrap()),
        ))
        .unwrap();
        self.config_uid = Some(observed["metadata"]["uid"].as_str().unwrap().to_owned());
        assert_eq!(observed["data"], config["data"]);
        let pod = self.pod(tier);
        let observed: Value = serde_json::from_slice(&run(
            &tier.health,
            &mut tier.kubectl(&["create", "-f", "-", "-o", "json"]),
            Some(&serde_json::to_vec(&pod).unwrap()),
        ))
        .unwrap();
        self.pod_uid = Some(observed["metadata"]["uid"].as_str().unwrap().to_owned());
        self.creation_inflight = false;
        run(
            &tier.health,
            &mut tier.kubectl(&[
                "wait",
                "--for=condition=Ready",
                &format!("pod/{}", self.name),
                "--timeout=120s",
            ]),
            None,
        );
        self.verify(tier);
    }

    fn configuration(&self, tier: &Tier) -> Value {
        let root = &tier.root;
        let contract = store::postgres::native_origin::release_contract();
        let path = root.join("k8s/postgres/restore-drill/native-origin-contract.json");
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(path).unwrap()).unwrap(),
            contract
        );
        let mut data = serde_json::Map::new();
        for (name, path) in [
            (
                "base-backup.sh",
                "k8s/postgres/backup/scripts/base-backup.sh",
            ),
            (
                "restore.sh",
                "k8s/postgres/restore-drill/scripts/restore.sh",
            ),
            ("verify.sql", "k8s/postgres/restore-drill/verify.sql"),
        ] {
            let contents = std::fs::read_to_string(root.join(path)).unwrap();
            // These are the local physical scripts, never the fetch/upload cloud transport.
            for forbidden in ["gcloud", "gs://", "curl ", "wget ", "https://"] {
                assert!(
                    !contents.contains(forbidden),
                    "cloud transport is forbidden in this owned local drill"
                );
            }
            data.insert(name.into(), json!(contents));
        }
        data.insert(
            "native-origin-contract.json".into(),
            json!(serde_json::to_string(&contract).unwrap()),
        );
        data.insert("receiver.sh".into(), json!(include_str!("receiver.sh")));
        json!({"apiVersion":"v1","kind":"ConfigMap","immutable":true,
            "metadata":{"name":self.name,"namespace":"navigator","labels":{(OWNER_LABEL):&tier.operation}},"data":data})
    }

    fn pod(&self, tier: &Tier) -> Value {
        let image = &tier.template["spec"]["template"]["spec"]["containers"][0]["image"];
        json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":self.name,"namespace":"navigator",
            "labels":{(OWNER_LABEL):&tier.operation,"navigator.neonlaw.org/postgres-client":"true"}},
          "spec":{"restartPolicy":"Never","activeDeadlineSeconds":1800,"automountServiceAccountToken":false,
            "securityContext":{"runAsNonRoot":true,"runAsUser":999,"runAsGroup":999,"fsGroup":999,"seccompProfile":{"type":"RuntimeDefault"}},
            "containers":[{"name":"recovery","image":image,"command":["/bin/sh","/scripts/receiver.sh",format!("physical_{}",tier.operation)],
              "env":[{"name":"PGHOST","value":"navigator-postgres"},{"name":"PGPORT","value":"5432"},
                {"name":"PGUSER","value":"navigator_backup"},{"name":"PGAPPNAME","value":format!("physical_{}",tier.operation)},
                {"name":"PGSSLMODE","value":"verify-full"},{"name":"PGSSLROOTCERT","value":"/etc/backup/ca.crt"},
                {"name":"PGPASSWORD","valueFrom":{"secretKeyRef":{"name":"navigator-postgres-backup","key":"password"}}}],
              "securityContext":{"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,"capabilities":{"drop":["ALL"]}},
              "resources":{"requests":{"cpu":"250m","memory":"256Mi","ephemeral-storage":"1Gi"},
                 "limits":{"memory":"2Gi","ephemeral-storage":"20Gi"}},
              "volumeMounts":[{"name":"work","mountPath":"/work"},{"name":"backup","mountPath":"/backup"},
                {"name":"wal","mountPath":"/wal"},{"name":"restore","mountPath":"/restore"},{"name":"tmp","mountPath":"/tmp"},
                {"name":"scripts","mountPath":"/scripts","readOnly":true},
                {"name":"scripts","mountPath":"/etc/navigator/postgres/drill","readOnly":true},
                {"name":"credentials","mountPath":"/etc/backup","readOnly":true},
                {"name":"tls","mountPath":"/etc/postgres/tls","readOnly":true},
                {"name":"config","mountPath":"/etc/postgres/config","readOnly":true}]}],
            "volumes":[{"name":"work","emptyDir":{}},{"name":"backup","emptyDir":{}},{"name":"wal","emptyDir":{}},
              {"name":"restore","emptyDir":{}},{"name":"tmp","emptyDir":{}},
              {"name":"scripts","configMap":{"name":self.name}},
              {"name":"credentials","secret":{"secretName":"navigator-postgres-backup","defaultMode":416}},
              {"name":"tls","secret":{"secretName":"navigator-postgres-tls","defaultMode":416}},
              {"name":"config","configMap":{"name":"navigator-postgres-config"}}]}})
    }

    pub fn stop_receiver(&self, tier: &Tier) {
        self.exec(tier,&["/bin/sh","-ec",
            "test ! -f /work/receiver-drained || exit 0; \
             test -s /work/receiver.pid; touch /work/receiver-stop-request; \
             kill -TERM \"$(cat /work/receiver.pid)\"; \
             for n in $(seq 1 100); do test ! -f /work/receiver-drained || exit 0; sleep .1; done; exit 1"]);
    }

    pub fn delete(&self, tier: &Tier) {
        assert!(!self.creation_inflight);
        if let Some(uid) = &self.pod_uid {
            self.verify(tier);
            let options =
                json!({"apiVersion":"v1","kind":"DeleteOptions","preconditions":{"uid":uid}});
            run(
                &tier.health,
                &mut tier.kubectl(&[
                    "delete",
                    "--raw",
                    &format!("/api/v1/namespaces/navigator/pods/{}", self.name),
                    "-f",
                    "-",
                ]),
                Some(&serde_json::to_vec(&options).unwrap()),
            );
            let until = Instant::now() + Duration::from_secs(45);
            loop {
                let all = tier.json(&["get", "pods", "-o", "json"]);
                if !all["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|pod| pod["metadata"]["uid"] == *uid)
                {
                    break;
                }
                assert!(
                    Instant::now() < until,
                    "scratch Pod drain unproved; retain dependent resources"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if let Some(uid) = &self.config_uid {
            tier.delete_uid("configmaps", &self.name, uid);
        }
    }
}
