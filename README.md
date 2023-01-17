# SDP Kubernetes Injector
SDP Kubernetes Injector provides **egress access**, from Kubernetes workloads to protected resources behind an SDP Gateway, using [sidecar](https://kubernetes.io/docs/concepts/workloads/pods/#workload-resources-for-managing-pods) [injector](https://kubernetes.io/docs/reference/access-authn-authz/extensible-admission-controllers/) pattern.

The Injector automatically manages identities for the injected Clients and enables Policy assignment based on Kubernetes labels.

For ingress access, from external clients to SDP Gateway protected workloads in a Kubernetes cluster, use [URL access](https://sdphelp.appgate.com/adminguide/v6.1/defining-hosts.html?anchor=url-access) instead.

## Requirements
### Tool Requirements
The following tools are required to install the SDP Kubernetes Injector:
* [helm v3.8.0+](https://helm.sh/docs/intro/install)
* [kubectl](https://kubernetes.io/docs/tasks/tools/#kubectl)

### SDP Requirements
SDP Kubernetes Injector requires following configurations on the SDP Controller:
* Service User License
  * 1 Service User license is consumed per Kubernetes workload (e.g. Deployment).
  * 10 inactive Service Users are created at the initialization of the Identity Service.
* IP Pool assigned on `service` Identity Provider
  * 1 IP is assigned from the IP Pool for every Pod in the Kubernetes workload.
  * Each Pod is assigned a Device ID by the Injector's Device ID service. Device IDs are persistent so will survive Pod restarts.
* API User with required privileges
  * Admin Role with `Service User Management Preset` in the Admin UI.
  * See [Injector](#injector) section for individual privileges required.
* **(Optional)** Client User if the Controller API is protected behind SDP.
  * The user will be used by the SDP Client running inside the Injector itself (referred as "meta-client") to connect Controller API.
  * Policy/Entitlement to access the DNS.
    * `ALLOW TCP up to <INTERNAL DNS IP> - port 53`
    * `ALLOW UDP up to <INTERNAL DNS IP> - port 53`
  * Policy/Entitlement to access the Controller API:
    * `ALLOW TCP up to <CONTROLLER ADMIN INTERFACE> - port 8443`
  * See [Meta-Client](#meta-client-sdp-client-for-the-injector) for details.

### Kubernetes Requirements
* SDP Kubernetes Injector requires [cert-manager](https://cert-manager.io/) to be deployed in the Kubernetes Cluster.
  * cert-manager is used to create the necessary certificate for the Injector's webhook only.

## Getting Started
### Installation
Currently the only supported way of installing the Injector is to use the official [Helm charts](https://helm.sh/docs/topics/charts/).

1.  Configure the SDP Collective per [SDP Requirements](#sdp-requirements). Verify:
    - Service User License
    - An IP Pool assigned to `service` Identity Provider
    - An API user with `Service User Management Preset` privileges
1.  Check if a supported version of the cert-manager (1.9 or newer) is already installed.
    ```shell
    $ kubectl get pods --namespace cert-manager -o jsonpath="{.items[*].spec.containers[*].image}"
    ```
    If not, install [cert-manager](https://cert-manager.io/docs/installation/helm/) and its CRDs with Helm.

    ```shell
    $ helm repo add jetstack https://charts.jetstack.io
    $ helm repo update
    $ helm install cert-manager jetstack/cert-manager \
      --namespace cert-manager \
      --create-namespace \
      --set installCRDs=true
    ```
1.  Install the SDP Kubernetes Injector CRD with Helm.
    ```shell
    $ helm install sdp-k8s-injector-crd oci://ghcr.io/appgate/charts/sdp-k8s-injector-crd \
        --namespace sdp-system \
        --create-namespace
    ```
1.  Create a Secret containing the credentials for the Controller API user with a descriptive name, for example: `sdp-injector-api-secret`.
    ```shell
    $ kubectl create secret generic sdp-injector-api-secret \
      --namespace sdp-system \
      --from-literal=sdp-injector-api-username="<USERNAME>" \
      --from-literal=sdp-injector-api-password="<PASSWORD>" \
      --from-literal=sdp-injector-api-provider="<PROVIDER>"
    ```
1.  Generate a `values.yaml` file. Below is an example of the minimum required configuration. For other parameters, see [Parameters](#parameters)
    ```yaml
    # values.yaml
    sdp:
      host: https://controller.company.com:8443 # controller admin interface hostname
      adminSecret: sdp-injector-api-secret      # api credentials secret name from previous step
      clusterID: k8s-prod                       # cluster id, will be visible in the Admin UI
    ```
1.  Install the SDP Kubernetes Injector with Helm.
    ```shell
    $ helm install sdp-k8s-injector oci://ghcr.io/appgate/charts/sdp-k8s-injector \
      --namespace sdp-system \
      --values values.yaml
    ```
1.  Verify the installation.
    1.  There should be three pods running in `sdp-system` namespace.
        ```shell
        $ kubectl get pods --namespace sdp-system

        NAME                                                  READY   STATUS    RESTARTS   AGE
        sdp-k8s-injector-device-id-service-56c5dff485-s8f6f   1/1     Running   0          3m40s
        sdp-k8s-injector-identity-service-67b847bd6-x5jst     1/1     Running   0          3m40s
        sdp-k8s-injector-injector-6f7748f888-v88dh            1/1     Running   0          3m40s
        ```
    2.  There should be 10 disabled service users in the Admin UI, tagged with `k8s`.
        https://controller.company.com:8443/ui/identity/service-users
    3.  Injector configuration is verified. To test the egress access, see [Testing the Installation](#testing-the-installation).

> Browse the available versions on [Appgate GitHub Container Registry](https://github.com/appgate/sdp-k8s-injector/pkgs/container/charts%2Fsdp-k8s-injector)

### Testing the Installation

This section explains how to test egress access, end to end. We'll create a dedicated namespace, create a Deployment (named pingtest) and give the Pod in that Deployment access to a protected resources.

Note that this will consume 1 Service User license.

We'll assume there is an ICMP Entitlement for a test server: `192.168.111.5`

1.  Create an Access Policy named "PingTest K8s Workload".
    - Use following criteria for assignment:
      - **Identity Provider** is `service`
      - **labels** expression `namespace === "sdp-demo"`
      - **labels** expression `name === "pingtest"`
1.  Create an example namespace `sdp-demo` in the cluster and label it with `sdp-injection="enabled"`.
    ```shell
    $ kubectl create namespace sdp-demo
    $ kubectl label namespace sdp-demo --overwrite sdp-injection="enabled"
    ```
    With the `sdp-injection` label, the Injector will monitor this namespace. The default strategy is to inject SDP Client to every Deployment, see [Default Injection Strategy](#default-injection-strategy) for details.
1.  Create a [busybox](https://hub.docker.com/_/busybox) Deployment in the same namespace and verify the following checklist:
    ```shell
    $ kubectl create deployment pingtest --namespace sdp-demo --image=busybox --replicas=1 -- sleep infinity
    ```
    Note that the Injector works with [Deployments](https://kubernetes.io/docs/concepts/workloads/controllers/deployment/) only, creating Pods directly is not supported.
1.  Verify the access.

    From the pod, get a shell first then ping:
    ```shell
    $ kubectl -n sdp-demo exec -it $(kubectl get pod -n sdp-demo -l app=pingtest -o name --no-headers) -- sh
    $ # ip route | grep tun0
    $ # ping 192.168.111.5
    ```
    From the Admin UI:
    Go to Home > Active Sessions > k8s-prod_sdp-demo_pingtest and review the claims and entitlements.

1.  If access is not working, see [Troubleshooting](#troubleshooting).

1.  Cleanup the test resources.
    ```shell
    $ kubectl delete namespace sdp-demo
    ```
    Note that the license will be cleanup after 30 days automatically.

## Usage
### Setting Policy for Deployments
The Injector allows SDP Controller to become aware of labels in Kubernetes. By default, the Deployment name and namespace are exposed to SDP as `user.labels.name` and `user.labels.namespace` respectively.

Assume that we have an Injector installed in the cluster with `clusterID=demo` and we have also created a Deployment `sleep-forever` like below in the `sdp-demo` namespace.

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: sleep-forever
  namespace: sdp-demo
spec:
  replicas: 1
  selector:
  matchLabels:
    app: sleep-forever
  template:
  spec:
    containers:
    - name: ubuntu
    image: ubuntu:latest
    command: [ "/bin/bash", "-c", "--" ]
    args: [ "while true; do sleep 30; done;" ]
```

To create a Policy for this deployment, set the following Assignments:
* Identity Provider is `service`
* `user.labels.name` === `sleep-forever` (labels - Expression: `name === "sleep-forever"`)
* `user.labels.namespace` === `sdp-demo` (labels - Expression: `namespace === "sdp-demo"`)
* `The array tags has item matching demo` (tags - Operator: `match` Value: `demo`)

In addition to the name and namespace, the Injector is able to expose `metadata.labels` as conditions for Assignment. For example, if we add `metadata.labels.role="sleeper"` to the example above, that label will be available as `users.labels.role` in the Policy. You can additionally set the following Assignment:
* `users.labels.role` === `sleeper` (labels - Expression: `role === "sleeper"`)

## Advanced Usage
### Namespace Labels
SDP injection is bound to namespaces. Adding the label `sdp-injection="enabled"` to a namespace will instruct the Injector to inject a sidecar to all pods in that namespace.
```shell
$ kubectl label --overwrite namespace sdp-demo sdp-injection="enabled"
```
Check the label of the namespace to see if injection is enabled
```shell
$ kubectl get namespace sdp-demo -L sdp-injection
NAME          STATUS   AGE    SDP-INJECTION
sdp-demo      Active   1m     enabled
```

### Meta-Client (SDP Client for the Injector)
The Injector (particularly, Identity Service) requires a connection to the Controller API, you may have the Controller API protected via SDP. In that case, you can use the meta-client feature which loads an SDP Client next to the Identity Service so the Injector can establish a connection to the Controller API.

To use the meta-client, you need the following:
* Secret with the following keys:
  * `sdp-injector-mc-username` - Username
  * `sdp-injector-mc-password` - Password
  * `sdp-injector-mc-profile-url` - Profile URL
* ConfigMap with the following keys:
  * `sdp-injector-mc-log-level` - Log level of the meta-client
  * `sdp-injector-mc-device-id` - Device ID (UUID v4) of the meta-client

```shell
$ kubectl create secret generic sdp-injector-mc-secret --namespace sdp-system \
  --from-literal=sdp-injector-mc-username="<USERNAME>" \
  --from-literal=sdp-injector-mc-password="<PASSWORD>" \
  --from-literal=sdp-injector-mc-profile-url="<URL>"
```

```shell
$ kubectl create configmap sdp-injector-mc-config --namespace sdp-system \
  --from-literal=sdp-injector-mc-log-level="<LOG_LEVEL>" \
  --from-literal=sdp-injector-mc-device-id="<UUID>"
```

Additionally, you must provide an Entitlement for this user on the Controller:
* Admin Policy/Entitlement to access Controller API:
  * `ALLOW TCP up to <CONTROLLER ADMIN INTERFACE> - port 8443`
* DNS Policy/Entitlement to resolve Controller API:
  * DNS Configuration
    * Match Domain: `<DOMAIN>`
    * DNS Servers: `<INTERNAL DNS IP>`
  * DNS Entitlement
    * `ALLOW TCP up to <INTERNAL DNS IP> - port 53`
    * `ALLOW UDP up to <INTERNAL DNS IP> - port 53`

> Note: User for the meta-client can be the same user as the Injector user

After creating the secret/configmap and configuring the policy/entitlement on SDP, provide the name of these resources to the helm chart and set `sdp.metaClient.enabled=true`.

```yaml
sdp:
  metaClient:
  enabled: true
  adminSecret: sdp-injector-mc-secret
  adminConfig: sdp-injector-mc-config
```

Upon installation of the chart, this Secret and ConfigMap will be passed to the sidecar injected next to the Identity Service.

### Default Injection Strategy
You can specify the default injection strategy for a given namespace by specifying the annotation `k8s.appgate.com/sdp-injector.strategy`.
There are two supported types of strategy:
1. `enabledByDefault` - Inject sidecars to all pods created within the namespace.
2. `disabledByDefault` - Do not inject sidecars to pods automatically.

Annotate the namespace with `k8s.appgate.com/sdp-injector.strategy=<STRATEGY>` to set strategy. If the annotation is not set on the namespace, it will use `enabledByDefault` as its default strategy.

```shell
$ kubectl annotate namespace k8s.appgate.com/sdp-injector.enabled="true"
$ kubectl annotate namespace k8s.appgate.com/sdp-injector.strategy="enabledByDefault"
```

To disable injection at a per-deployment basis in a namespace annotated with `enabledByDefault`, annotate the Deployment with `k8s.appgate.com/sdp-injector.enabled="false"`.

```shell
$ kubectl annotate Deployment <DEPLOYMENT> k8s.appgate.com/sdp-injector.enabled="false"
```

To enable injection at a per-deployment basis in a namespace annotated with `disabledByDefault`, annotate each Deployment with `k8s.appgate.com/sdp-injector.enabled=true`.

```shell
$ kubectl annotate Deployment <DEPLOYMENT> k8s.appgate.com/sdp-injector.enabled="true"
```

### Alternative Client Versions
The Injector takes the helm value `sdp.clientVersion` as the default client version to use. By annotating a Pod or Deployment with `k8s.appgate.com/sdp-injector.client-version=<VERSION>`, the Injector will load an SDP Client version different from the default.

Assuming the default client version is 6.1, you can inject a 5.5 client at a per-deployment basis by annotating the Deployment with `k8s.appgate.com/sdp-injector.client-version=5.5`.
```shell
$ kubectl annotate Deployment <DEPLOYMENT> k8s.appgate.com/sdp-injector.client-version="5.5"
```

### Init Containers
When the sidecar injection is enabled and the Injector detects a new Pod with init-containers, it loads extra containers at the head (`sdp-init-container-0`) and tail (`sdp-init-container-f`) of the init-containers list.

The initial init-container `sdp-init-container-0` is meant to preserve the original DNS configuration of the Pod by setting the nameserver to the kube DNS IP address and the nameservers specified in the helm value `sdp.dnsmasq.dnsConfig.search`.

The last init-container `sdp-init-container-f` overwrite `/etc/resolv.conf` by setting the nameserver to `127.0.0.1` so the Pod can use the DNS server provided by the Injector's dnsmasq.

You can disable the injection of these init-containers by annotation the Deployment with `k8s.appgate.com/sdp-injector.strategy.disable-init-containers="true"`.
```shell
$ kubectl annotate Deployment <DEPLOYMENT> k8s.appgate.com/sdp-injector.disable-init-containers="true"
```

### Multiple Clusters
You can connect multiple Kubernetes clusters to a single SDP system by installing an injector on each cluster. When installing the Injector, set a unique cluster ID in the helm value `sdp.clusterID`. To prevent collision of resources created by the Injector, the SDP system will use this ID as a tag or prefix (e.g. Client Profiles, Service Users). It is advised to tag your admin users for each injector with the cluster ID.

## Annotations
SDP Kubernetes Injector supports various annotation-based behavior customization

| Annotation                                             | Available Options                                                          | Description                                                                                                                                                                                                                                                                                                                                                                                                                        |
|--------------------------------------------------------|----------------------------------------------------------------------------|:-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `k8s.appgate.com/sdp-injector.strategy`                | `enabledByDefault`, `disabledByDefault`                                    | Defines the default injection strategy of the namespace. Use this annotation with `k8s.appgate.com/sdp-injector.enabled`. If `enabledByDefault`, the Injector will always inject sidecars to deployment. If `disabledByDefault`, the Injector will only inject sidecars to deployments annotated with `k8s.appgate.com/sdp-injector.enabled`. If the annotation is not specified in the namespace, it will use `enabledByDefault`. |
| `k8s.appgate.com/sdp-injector.enabled`                 | `true`, `false`                                                            | Defines whether the sidecar can be injected in the pod. Use this annotation with `k8s.appgate.com/sdp-injector.strategy`. In a `enabledByDefault` namespace, the default value will be `true`. In a `disabledByDefault` namespace, the default value will be `false`.                                                                                                                                                              |
| `k8s.appgate.com/sdp-injector.client-version`          | `5.5`, `6.0`, `6.1`                                                        | Specifies the SDP Client version to inject as a sidecar. The default client version is specified by the helm value `sdp.clientVersion`. When this annotation is used on a deployment, it will override the helm value.                                                                                                                                                                                                             |
| `k8s.appgate.com/sdp-injector.disable-init-containers` | `true`, `false`                                                            | When `initContainers` are present in a pod, the Injector loads extra init-containers for DNS resolution. This annotation will disable the injection of init-containers if set to `false`                                                                                                                                                                                                                                           |
| `k8s.appgate.com/sdp-injector.custom-dns-search`       | Space-separated string of domains (e.g. `svc.cluster.local cluster.local`) | Additional domains to the list of domains in the DNS resolution.                                                                                                                                                                                                                                                                                                                                                                   |

## Parameters

### SDP parameters

| Name                                      | Description                                                                              | Value                                   |
| ----------------------------------------- | ---------------------------------------------------------------------------------------- | --------------------------------------- |
| `global.image.repository`                 | Image registry to use for all SDP images.                                                | `ghcr.io/appgate/sdp-k8s-injector`      |
| `global.image.tag`                        | Image tag to use for all SDP images. If not set, it defaults to `.Chart.appVersion`.     | `""`                                    |
| `global.image.pullPolicy`                 | Image pull policy to use for all SDP images.                                             | `IfNotPresent`                          |
| `global.image.pullSecrets`                | Image pull secret to use for all SDP images.                                             | `[]`                                    |
| `sdp.host`                                | Hostname of the SDP controller                                                           | `""`                                    |
| `sdp.adminSecret`                         | Name of the secret for initial authentication                                            | `""`                                    |
| `sdp.clientVersion`                       | Version of the SDP client to inject as sidecars.                                         | `6.1`                                   |
| `sdp.clusterID`                           | An identifier to prefix service users and client profiles                                | `""`                                    |
| `sdp.metaClient.enabled`                  | Whether to set up an SDP client on the Identity Service                                  | `false`                                 |
| `sdp.metaClient.adminSecret`              | Name of the secret for initial authentication                                            | `""`                                    |
| `sdp.metaClient.adminConfig`              | Name of the config for initial authentication                                            | `""`                                    |
| `sdp.metaClient.dnsService`               | IP of the kube-dns service                                                               | `""`                                    |
| `sdp.metaClient.dnsConfig.searches`       | Search domains to add to the Pod DNS configuration                                       | `["svc.cluster.local","cluster.local"]` |
| `sdp.injector.logLevel`                   | SDP Injector log level.                                                                  | `info`                                  |
| `sdp.injector.replica`                    | Number of Device ID Service replicas to deploy                                           | `1`                                     |
| `sdp.injector.certificatePollingInterval` | Polling interval in seconds to watch for changes to the certificate                      | `120`                                   |
| `sdp.injector.image.repository`           | SDP Injector image repository. If set, it overrides `.global.image.repository`.          | `""`                                    |
| `sdp.injector.image.tag`                  | SDP Injector image tag. If set, it overrides `chart.appVersion`.                         | `""`                                    |
| `sdp.injector.image.pullPolicy`           | SDP Injector pull policy. If set, it overrides `.global.image.pullPolicy`.               | `Always`                                |
| `sdp.deviceIdService.logLevel`            | SDP Device ID Service log level.                                                         | `info`                                  |
| `sdp.deviceIdService.replica`             | Number of SDP Device ID Service replicas to deploy                                       | `1`                                     |
| `sdp.deviceIdService.image.repository`    | SDP Device ID Service image repository. If set, it overrides `.global.image.repository`. | `""`                                    |
| `sdp.deviceIdService.image.tag`           | SDP Device ID Service image tag. If set, it overrides `.chart.appVersion`.               | `""`                                    |
| `sdp.deviceIdService.image.pullPolicy`    | SDP Device ID Service pull policy. If set, it overrides `.global.image.pullPolicy`.      | `Always`                                |
| `sdp.identityService.logLevel`            | SDP Identity Service log level.                                                          | `info`                                  |
| `sdp.identityService.replica`             | Number of SDP Identity Service replicas to deploy                                        | `1`                                     |
| `sdp.identityService.image.repository`    | SDP Identity Service image repository. If set, it overrides `.global.image.repository`.  | `""`                                    |
| `sdp.identityService.image.tag`           | SDP Identity Service image tag. If set, it overrides `.chart.appVersion`.                | `""`                                    |
| `sdp.identityService.image.pullPolicy`    | SDP Identity Service pull policy. If set, it overrides `.global.image.pullPolicy`.       | `Always`                                |
| `sdp.headlessService.image.tag`           | SDP Headless Service image repository. If set, it overrides `.global.image.repository`.  | `""`                                    |
| `sdp.headlessService.image.repository`    | SDP Headless Service image tag. If set, it overrides `.sdp.clientVersion`.               | `""`                                    |
| `sdp.headlessService.image.pullPolicy`    | SDP Headless Service image pull policy. If set, it overrides `.global.image.pullPolicy`. | `Always`                                |
| `sdp.headlessDriver.image.repository`     | SDP Headless Driver image repository. If set, it overrides `.global.image.repository`.   | `""`                                    |
| `sdp.headlessDriver.image.tag`            | SDP Headless Driver image tag. If set, it overrides `sdp.clientVersion`.                 | `""`                                    |
| `sdp.headlessDriver.image.pullPolicy`     | SDP Headless Service image pull policy. If set, it overrides `.global.image.pullPolicy`. | `Always`                                |
| `sdp.dnsmasq.image.repository`            | SDP Dnsmasq image repository. If set, it overrides `.global.image.repository`.           | `""`                                    |
| `sdp.dnsmasq.image.tag`                   | SDP Dnsmasq image tag. If set, it overrides `sdp.clientVersion`.                         | `""`                                    |
| `sdp.dnsmasq.image.pullPolicy`            | SDP Dnsmasq image pull policy. If set, it overrides `.global.image.pullPolicy`.          | `Always`                                |
| `sdp.dnsmasq.dnsConfig.searches`          | Search domains to add to the Pod DNS configuration                                       | `["svc.cluster.local","cluster.local"]` |


### Kubernetes parameters

| Name           | Description         | Value       |
| -------------- | ------------------- | ----------- |
| `service.type` | Type of the service | `ClusterIP` |
| `service.port` | Port of the service | `443`       |


This table above was generated using [readme-generator-for-helm](https://github.com/bitnami-labs/readme-generator-for-helm)


## How It Works
### Overview
SDP Kubernetes Injector consists of three components:
* Identity Service
* Device ID Service
* Injector

### Identity Service
SDP Identity Service is mainly responsible for the management of the Service User credentials. It consists of three subcomponents:
* Deployment Watcher
* Identity Creator
* Identity Manager

As the name implies, **Deployment Watcher** continuously monitors for the creation of Deployment in the namespace labeled for sidecar injection. **Identity Creator** communicates with the SDP system to generate SDP system and maintains an in-memory pool of Service User credentials. **Identity Manager** facilitates the messaging between these subcomponents.

When the SDP Identity Service is initialized, the Identity Creator immediately creates Service Users on the SDP system and stores them as inactive credentials in its in-memory pool. When the Deployment Watcher discovers a newly created Deployment eligible for injection, it requests the Identity Manager to create a new ServiceIdentity. Upon creating a new ServiceIdentity, the Identity Manager instructs the Identity Creator to activate the corresponding Service User credentials which generates a Secret containing the Service User credentials in the deployment's namespace. This Secret is, later, mounted in the Pod and its credentials exposed as environment variables to the sidecar container.

### Device ID Service
Device ID Service is responsible for assigning UUIDs to each Pod in the deployment
* Service Identity Watcher
* Device ID Manager

When a new ServiceIdentity is created by the Identity Service, the **Service Identity Watcher** notifies the **Device ID Manager** to generate a DeviceID. For every Pod in the Deployment (defined by .spec.replica), the manager generates a UUID and stores it in the Device ID.

### Injector
Injector is an admission webhook server that mutates Pod creation requests. By registering a [Mutating Admission Webhook](https://kubernetes.io/docs/reference/access-authn-authz/extensible-admission-controllers/) allows the Injector intercept all Pod creation requests in sdp-injection enabled namespace and patch the necessary configurations to enable egress traffic from Kubernetes workloads to resources protected by SDP.

When patching the pod, the Injector reads the ServiceIdentity and DeviceID (created by the aforementioned service) to inject the correct credentials and device ID into the pod.

Injector uses Controller API for managing identities of injected Clients, so it needs an Admin Role with `Service User Management Preset`, which includes the following privileges:
* View all Service Users
* Create all Service Users
* Edit all Service Users
* Delete all Service Users
* View all Client Profiles
* Create all Client Profiles
* Edit all Client Profiles
* Delete all Client Profiles
* Export all Client Profiles

### sdp-dnsmasq
SDP Clients can make DNS queries for specific hosts to specific DNS servers behind the SDP Gateways. This is configured by the sdp-driver which notifies the system about which domains should use the DNS servers behind the SDP Gateways.

In the case of the SDP Kubernetes Injector, a dnsmasq instance is configured according to the instructions that the sdp-driver sends. Since only sdp-driver container can modify `/etc/resolv.conf`, the setup is done in the following steps:

1.  The sdp-dnsmasq container grabs the address of the kube-dns service and starts a new dnsmasq instance using that DNS server as upstream. This allows the dnsmasq instance to forward everything into the kube-dns service.
2.  The sdp-dnsmasq container opens a UNIX socket to receive instructions from the sdp-driver container for when there are specific domain based DNS settings.
3.  The sdp-driver container waits for the service to connect. Once connected, the sdp-driver calls the [sdp-driver-set-dns](./assets/sdp-driver-set-dns) script.
4.  [sdp-driver-set-dns](./assets/sdp-driver-set-dns) configures `/etc/resolv.conf` to point to sdp-dnsmasq. From this point onwards, sdp-dnsmasq takes the responsibility of resolving names inside the pod.
5.  Any new instructions are sent to sdp-dnsmasq via UNIX socket by the sdp-driver. Then sdp-dnsmasq configures dnsmasq with the latest DNS domain and DNS server updates.

## Known Issues
### Google Kubernetes Engine (GKE)
When running on GKE, the firewall needs to be configured to allow traffic from the Kubernetes API into the nodes to the port 8443 even if the service is listening. See [issue on GitHub](https://github.com/istio/istio/issues/19532).

## Troubleshooting
### Injector Problems

The Injector function may fail due to various reasons such as missing secrets, wrong api credentials, unaccessible Controller API.

Make sure that all three pods are running.
```shell
$ kubectl get pods -n sdp-system
NAME                                                  READY   STATUS    RESTARTS   AGE
sdp-k8s-injector-device-id-service-56c5dff485-s8f6f   1/1     Running   0          47h
sdp-k8s-injector-identity-service-67b847bd6-x5jst     1/1     Running   0          47h
sdp-k8s-injector-injector-6f7748f888-v88dh            1/1     Running   0          47h
```

Check the logs from the Injector pods:
```shell
$ kubectl logs sdp-k8s-injector-device-id-service-56c5dff485-s8f6f -n sdp-system
...

```
```shell
$ kubectl logs sdp-k8s-injector-identity-service-67b847bd6-x5jst -n sdp-system
...
```

```shell
$ kubectl logs sdp-k8s-injector-injector-6f7748f888-v88dh -n sdp-system
...
```

### Pod Access Problems

Pod egress access may fail due to various reasons such as insufficient Service User license, missing IP Pool assignment in the Service Identity Provider, no Entitlements due to wrong Policy configuration.

Make sure that all pods are running.
```shell
$ kubectl get pods -n sdp-demo
NAME                       READY   STATUS    RESTARTS   AGE
pingtest-c66bb48f9-s7n8j   4/4     Running   0          2m30s
```

Check the SDP Client logs, in all 3 sidecar containers: sdp-service, sdp-driver, sdp-dnsmasq

```shell
$ kubectl logs pingtest-c66bb48f9-s7n8j sdp-service -n sdp-demo
```

```shell
$ kubectl logs pingtest-c66bb48f9-s7n8j sdp-driver -n sdp-demo
```

```shell
$ kubectl logs pingtest-c66bb48f9-s7n8j sdp-dnsmasq -n sdp-demo
```

# Support

You can open a [github issue](https://github.com/appgate/sdp-k8s-injector/issues) or contact support@appgate.com