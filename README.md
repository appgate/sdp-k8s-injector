# SDP Kubernetes Client
SDP Kubernetes Client is a member of the Appgate SDP Client family that enables it to be used in Kubernetes clusters. By injecting a sidecar into pods on-demand, egress traffic from Kubernetes workloads can now be captured and sent to protected resources behind an SDP Gateway. It captures traffic in much the same way as other SDP Clients. The Entitlements will be defined in the Policy which in this case is likely to be assigned based on the use of the 'Services' identity provider combined with any labels that were used when the SDP Kubernetes Client was injected.

Remember you can already control ingress access to specific Kubernetes workloads using the URL access feature (HTTP up action type).

## Requirements

### Tool Requirements
The following tools are required to install the SDP Kubernetes Client:
* helm v3.7.0+ - https://helm.sh/docs/intro/install/
* kubectl - https://kubernetes.io/docs/tasks/tools/#kubectl

### SDP Requirements
SDP Kubernetes Client requires several configuration on the SDP Controller:
* Service User License
  * 1 Service User license is consumed per Kubernetes workload (e.g. Deployment)
  * 10 inactive Service Users are created at the initialization of the Identity Service
* Device ID
  * 1 IP is assigned from the IP Pool for every pod in the Kubernetes workload.
  * Each pod is assigned a UUID by the Device ID Service. UUIDs are reused if the pod is restarted.
  * Device IDs are only freed when they are unused for some amount of time defined in the IP Pool.
* User
  * `Service User Management Preset` for its Admin Role, which includes the following privileges:
    * View all Service Users
    * Create all Service Users
    * Edit all Service Users
    * Delete all Service Users
    * View all Client Profiles
    * Create all Client Profiles
    * Edit all Client Profiles
    * Delete all Client Profiles
    * Export all Client Profiles
  * If the Admin API is protected behind SDP, the user additionally needs
    * Policy/Entitlement to access the DNS
      * `ALLOW TCP up to <INTERNAL CONTROLLER IP> - port 53`
      * `ALLOW UDP up to <INTERNAL CONTROLLER IP> - port 53`
    * Policy/Entitlement to access the Admin API:
      * `ALLOW TCP up to <HOSTNAME> - port 8443`

## Getting Started
### Installation
> Browse the available versions on [Appgate GitHub Container Registry](https://github.com/appgate/sdp-k8s-client/pkgs/container/charts%2Fsdp-k8s-client)

1. Create `sdp-system` namespace for the SDP Kubernetes Client
    ```bash
     $ kubectl create namespace sdp-system
     ```
2. Install the SDP Kubernetes Client CRD with Helm
    ```bash
    $ export HELM_EXPERIMENTAL_OCI=1
    $ helm install sdp-k8s-client-crd oci://ghcr.io/appgate/charts/sdp-k8s-client-crd \
         --namespace sdp-system \
         --version <VERSION>
    ```
3. Create a secret containing the username and password for admin authentication
   ```bash
   $ kubectl create secret generic <SECRET> \
        --namespace sdp-system \
        --from-literal=sdp-k8s-client-username="<USERNAME>" \
        --from-literal=sdp-k8s-client-password="<PASSWORD>" \
        --from-literal=sdp-k8s-client-provider="<PROVIDER>"
   ```
4. Install the SDP Kubernetes Client with Helm
    ```bash
    $ export HELM_EXPERIMENTAL_OCI=1
    $ helm install sdp-k8s-client oci://ghcr.io/appgate/charts/sdp-k8s-client \
        --namespace sdp-system \
        --version <VERSION> \
        --set .sdp.host="<SDP_HOSTNAME>" \
        --set .sdp.adminSecret="<SECRET>"
    ```
5. To test the sidecar injection, create an example namespace `sdp-demo` and label it with `sdp-injection="enabled"`
    ```bash
    $ kubectl create namespace sdp-demo
    $ kubectl label namespace sdp-demo --overwrite sdp-injection="enabled"
    ```
6. Create busybox deployment in the same namespace and verify:
   1. There is a route through the gateway (via tun0)
   2. A resource protected by SDP is reachable
    ```bash
    $ kubectl create deployment pingtest --namespace sdp-demo --image=busybox --replicas=1 -- sleep infinity
    $ kubectl exec -it $(kubectl get pod -n sdp-demo -l app=pingtest -o name --no-headers) -- sh
    $ /# ip route | grep tun0
    $ /# ping <IP_ADDRESS>
    ```
   
### Configuration
#### Setting Policy for Deployments 
SDP Kubernetes Client allows SDP controller to become aware of labels in Kubernetes. By default, the Deployment name and namespace are exposed to SDP as `user.labels.name` and `user.labels.namespace` respectively.

Assume that we have created the deployment `sleep-forever` below in the `sdp-demo` namespace which has injection enabled.
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

In addition to the name and namespace, the SDP Kubernetes Client is able to expose `metadata.labels` as conditions for Assignment. For example, if we add `metadata.labels.role="sleeper"` to the example above, that label will be available as `users.labels.role` in the Policy. You can additionally set the following Assignment:
* `users.labels.role` === `sleeper` (labels - Expression: `role === "sleeper"`)

## Advanced Usage
### Namespace Labels
SDP injection is bound to namespaces. Adding the label `sdp-injection="enabled"` to a namespace will instruct the SDP Kubernetes Client to inject a sidecar to all pods in the namespace.
```bash
$ kubectl label --overwrite namespace sdp-demo sdp-injection="enabled"
```
Check the label of the namespace to see if injection is enabled
```bash
$ kubectl get namespace sdp-demo -L sdp-injection
NAME          STATUS   AGE    SDP-INJECTION
sdp-demo      Active   1m     enabled
```

### Meta-Client (Sidecar for Injector)
Because the injector (particularly, Identity Service) requires a connection to the Controller API, you may want to hide the connection via SPA. In that case, you can use the meta-client feature which loads an SDP client next to the Identity Service so the injector can establish a connection to the controller via SPA. 

To use the meta-client, you need the following:
* Secret with the following keys:
  * `meta-client-username` - Username
  * `meta-client-password` - Password
  * `meta-client-provider` - Provider
  * `meta-client-profile-url` - Profile URL
* ConfigMap with the following keys:
  * `meta-client-log-level` - Log level of the meta-client
  * `meta-client-device-id` - Device ID (UUID v4) of the meta-client

```bash
$ kubectl create secret generic sdp-k8s-meta-client-secret --namespace sdp-system \
	--from-literal=meta-client-username="" \
	--from-literal=meta-client-password="" \
	--from-literal=meta-client-provider="" \
	--from-literal=meta-client-profile-url=""
```

```bash
$ kubectl create configmap sdp-k8s-meta-client-config --namespace sdp-system \
	--from-literal=meta-client-log-level=<LOG_LEVEL> \
	--from-literal=meta-client-device-id=<UUID>
```

Additionally, you must provide an Entitlement for this user on the controller:
* Admin Policy/Entitlement to access Admin APi:
  * `ALLOW TCP up to <HOSTNAME> - port 8443`
* DNS Policy/Entitlement to resolve Admin API: 
  * DNS Configuration
    * Match Domain: `<DOMAIN>`
    * DNS Servers: `<DNS_SERVER_IP>`
  * DNS Entitlement
    * `ALLOW TCP up to <DNS_SERVER_IP> - port 53`
    * `ALLOW UDP up to <DNS_SERVER_IP> - port 53`

> Note: User for the meta-client can be the same user as the injector user

After creating the secret/configmap and configuring the poliyc/entitlment on SDP, provide the name of these resources to the helm chart and set `sdp.metaClient.enabled=true`. 

```yaml
sdp:
  metaClient:
    enabled: true
    adminSecret: sdp-k8s-meta-client-secret
    adminConfig: sdp-k8s-meta-client-config
```

Upon installation of the chart, this secret and configmap will be passed to the sidecar injected next to the Identity Service.

### Default Injection Strategy
There are two types of strategy for `sdp-injector-strategy`:
1. `enabledByDefault` - Inject sidecars to all pods created within the namespace.
2. `disabledByDefault` - Do not inject sidecars to pods automatically.

Annotate the namespace with `sdp-injector-strategy=<STRATEGY>` to set strategy. If the annotation is not set on the namespace, it will use `enabledByDefault` as its default strategy.

```bash
$ kubectl annotate namespace sdp-injector-enabled="true"
$ kubectl annotate namespace sdp-injector-strategy="enabledByDefault"
```

To disable injection at a per-deployment basis in a namespace annotated with `enabledByDefault`, annotate the deployment with `sdp-injector-enabled="false"`.

To enable injection at a per-deployment basis in a namespace annotated with `disabledByDefault`, annotate each deployment with `sdp-injector-enabled=true`.

### Alternative Client Versions
The injector takes the helm value `sdp.clientVersion` as the default client version to use. By annotating a pod or deployment with `sdp-injector-client-version=<VERSION>`, the injector will load an SDP client version different from the default.

Assuming the default client version is 6.x.x, you can inject a 5.x.x client by annotating the pod with `sdp-injector-client-version=5.x.x`.
```bash
$ kubectl annotate pod <POD> sdp-injector-client-version="5.5.1"
```

### Init Containers
When the sidecar injection is enabled and the injector detects a new pod with init-containers, it loads extra containers at the head (`sdp-init-container-0`) and tail (`sdp-init-container-f`) of the init-containers list. 

The initial init-container `sdp-init-container-0` is meant to preserve the original DNS configuration of the pod by setting the nameserver to the kube DNS IP address and the nameservers specified in the helm value `sdp.dnsmasq.dnsConfig.search`. 

The last init-container `sdp-init-container-0` overwrite `/etc/resolv.conf` by setting the nameserver to `127.0.0.1` so the pod can use the DNS server provided by the injector's dnsmasq. 

You can disable the injection of these init-containers by providing the annotation `sdp-injector-disable-init-containers="true"` or by providing the annotation in the deployment's pod template. 
```bash
$ kubectl annotate pod <POD> sdp-injector-disable-init-containers="true"
```

### Multiple Injectors
You can connect multiple Kubernetes clusters to a single SDP system by installing an injector on each cluster. When installing the injector, set a unique cluster ID in the helm value `sdp.clusterID`. To prevent collision of resources created by the injector, the SDP system will use this ID as a tag or prefix (e.g. client profiles, service users).

## Parameters

### SDP parameters

| Name                                   | Description                                                                              | Value                                   |
|----------------------------------------|------------------------------------------------------------------------------------------|-----------------------------------------|
| `global.image.repository`              | Image registry to use for all SDP images.                                                | `ghcr.io/appgate/sdp-k8s-client`        |
| `global.image.tag`                     | Image tag to use for all SDP images. If not set, it defaults to `.Chart.appVersion`.     | `""`                                    |
| `global.image.pullPolicy`              | Image pull policy to use for all SDP images.                                             | `IfNotPresent`                          |
| `global.image.pullSecrets`             | Image pull secret to use for all SDP images.                                             | `[]`                                    |
| `cert-manager.installCRDs`             | Whether to install cert-manager CRDs.                                                    | `true`                                  |
| `sdp.host`                             | Hostname of the SDP controller                                                           | `""`                                    |
| `sdp.adminSecret`                      | Name of the secret for initial authentication                                            | `""`                                    |
| `sdp.clientVersion`                    | Version of the SDP client to inject as sidecars.                                         | `6.0.1`                                 |
| `sdp.clusterID`                        | An identifier to prefix service users and client profiles                                | `""`                                    |
| `sdp.metaClient.enabled`               | Whether to set up an SDP client on the Identity Service                                  | `false`                                 |
| `sdp.metaClient.adminSecret`           | Name of the secret for initial authentication                                            | `""`                                    |
| `sdp.metaClient.adminConfig`           | Name of the config for initial authentication                                            | `""`                                    |
| `sdp.metaClient.dnsService`            | IP of the kube-dns service                                                               | `""`                                    |
| `sdp.metaClient.dnsConfig.searches`    | Search domains to add to the Pod DNS configuration                                       | `["svc.cluster.local","cluster.local"]` |
| `sdp.injector.logLevel`                | SDP Injector log level.                                                                  | `info`                                  |
| `sdp.injector.replica`                 | Number of Device ID Service replicas to deploy                                           | `1`                                     |
| `sdp.injector.certDays`                | How many days will be the SDP Injector certificate be valid.                             | `365`                                   |
| `sdp.injector.image.repository`        | SDP Injector image repository. If set, it overrides `.global.image.repository`.          | `""`                                    |
| `sdp.injector.image.tag`               | SDP Injector image tag. If set, it overrides `chart.appVersion`.                         | `""`                                    |
| `sdp.injector.image.pullPolicy`        | SDP Injector pull policy. If set, it overrides `.global.image.pullPolicy`.               | `Always`                                |
| `sdp.deviceIdService.logLevel`         | SDP Device ID Service log level.                                                         | `info`                                  |
| `sdp.deviceIdService.replica`          | Number of SDP Device ID Service replicas to deploy                                       | `1`                                     |
| `sdp.deviceIdService.image.repository` | SDP Device ID Service image repository. If set, it overrides `.global.image.repository`. | `""`                                    |
| `sdp.deviceIdService.image.tag`        | SDP Device ID Service image tag. If set, it overrides `.chart.appVersion`.               | `""`                                    |
| `sdp.deviceIdService.image.pullPolicy` | SDP Device ID Service pull policy. If set, it overrides `.global.image.pullPolicy`.      | `Always`                                |
| `sdp.identityService.logLevel`         | SDP Identity Service log level.                                                          | `info`                                  |
| `sdp.identityService.replica`          | Number of SDP Identity Service replicas to deploy                                        | `1`                                     |
| `sdp.identityService.image.repository` | SDP Identity Service image repository. If set, it overrides `.global.image.repository`.  | `""`                                    |
| `sdp.identityService.image.tag`        | SDP Identity Service image tag. If set, it overrides `.chart.appVersion`.                | `""`                                    |
| `sdp.identityService.image.pullPolicy` | SDP Identity Service pull policy. If set, it overrides `.global.image.pullPolicy`.       | `Always`                                |
| `sdp.headlessService.image.tag`        | SDP Headless Service image repository. If set, it overrides `.global.image.repository`.  | `""`                                    |
| `sdp.headlessService.image.repository` | SDP Headless Service image tag. If set, it overrides `.sdp.clientVersion`.               | `""`                                    |
| `sdp.headlessService.image.pullPolicy` | SDP Headless Service image pull policy. If set, it overrides `.global.image.pullPolicy`. | `Always`                                |
| `sdp.headlessDriver.image.repository`  | SDP Headless Driver image repository. If set, it overrides `.global.image.repository`.   | `""`                                    |
| `sdp.headlessDriver.image.tag`         | SDP Headless Driver image tag. If set, it overrides `sdp.clientVersion`.                 | `""`                                    |
| `sdp.headlessDriver.image.pullPolicy`  | SDP Headless Service image pull policy. If set, it overrides `.global.image.pullPolicy`. | `Always`                                |
| `sdp.dnsmasq.image.repository`         | SDP Dnsmasq image repository. If set, it overrides `.global.image.repository`.           | `""`                                    |
| `sdp.dnsmasq.image.tag`                | SDP Dnsmasq image tag. If set, it overrides `sdp.clientVersion`.                         | `""`                                    |
| `sdp.dnsmasq.image.pullPolicy`         | SDP Dnsmasq image pull policy. If set, it overrides `.global.image.pullPolicy`.          | `Always`                                |
| `sdp.dnsmasq.dnsConfig.searches`       | Search domains to add to the Pod DNS configuration                                       | `["svc.cluster.local","cluster.local"]` |


### Kubernetes parameters

| Name           | Description         | Value       |
|----------------|---------------------|-------------|
| `service.type` | Type of the service | `ClusterIP` |
| `service.port` | Port of the service | `443`       |


This table above was generated using [readme-generator-for-helm](https://github.com/bitnami-labs/readme-generator-for-helm)


## How It Works
### Overview
SDP Kubernetes Client consists of three components:
* Identity Service
* Device ID Service
* Injector

### Identity Service
SDP Identity Service is mainly responsible for the management of the Service User credentials. It consists of three subcomponents:
* Deployment Watcher
* Identity Creator
* Identity Manager

As the name implies, **Deployment Watcher** continuously monitors for the creation of Deployment in the namespace labeled for sidecar injection. **Identity Creator** communicates with the SDP system to generate SDP system and maintains an in-memory pool of Service User credentials. **Identity Manager** facilitates the messaging between these subcomponents.

When the SDP Identity Service is initialized, the Identity Creator immediately creates Service Users on the SDP system and stores them as inactive credentials in its in-memory pool. When the Deployment Watcher discovers a newly created Deployment eligible for injection, it requests the Identity Manager to create a new ServiceIdentity. Upon creating a new ServiceIdentity, the Identity Manager instructs the Identity Creator to activate the corresponding Service User credentials which generates a secret containing the Service User credentials in the deployment's namespace. This secret is, later, mounted in the pod and its credentials exposed as environment variables to the sidecar container.

### Device ID Service
Device ID Service is responsible for assigning UUIDs to each pod in the deployment
* Service Identity Watcher
* Device ID Manager

When a new ServiceIdentity is created by the Identity Service, the **Service Identity Watcher** notifies the **Device ID Manager** to generate a DeviceID. For every pod in the deployment (defined by .spec.replica), the manager generates a UUID and stores it in the Device ID.

### Injector
Injector is an admission webhook server that mutates pod creation requests. By registering a [Mutating Admission Webhook](https://kubernetes.io/docs/reference/access-authn-authz/extensible-admission-controllers/) allows the injector intercept all pod creation requests in sdp-injection enabled namespace and patch the necessary configurations to enable egress traffic from Kubernetes workloads to resources protected by SDP.

When patching the pod, the Injector reads the ServiceIdentity and DeviceID (created by the aforementioned service) to inject the correct credentials and device ID into the pod.

### sdp-dnsmasq
SDP Clients can make DNS queries for specific hosts to specific DNS servers behind the SDP Gateways. This is configured by the sdp-driver which notifies the system about which domains should use the DNS servers behind the SDP Gateways.

In the case of the SDP Kubernetes Client, a dnsmasq instance is configured according to the instructions that the sdp-driver sends. Since only sdp-driver container can modify `/etc/resolv.conf`, the setup is done in the following steps:

1. The sdp-dnsmasq container grabs the address of the kube-dns service and starts a new dnsmasq instance using that DNS server as upstream. This allows the dnsmasq instance to forward everything into the kube-dns service.
2. The sdp-dnsmasq container opens a UNIX socket to receive instructions from the sdp-driver container for when there are specific domain based DNS settings.
3. The sdp-driver container waits for the service to connect. Once connected, the sdp-driver calls the [sdp-driver-set-dns](./assets/sdp-driver-set-dns) script.
4. [sdp-driver-set-dns](./assets/sdp-driver-set-dns) configures `/etc/resolv.conf` to point to sdp-dnsmasq. From this point onwards, sdp-dnsmasq takes the responsibility of resolving names inside the pod.
5. Any new instructions are sent to sdp-dnsmasq via UNIX socket by the sdp-driver. Then sdp-dnsmasq configures dnsmasq with the latest DNS domain and DNS server updates.

## Known Issues
### Google Kubernetes Engine (GKE)
When running on GKE, the firewall needs to be configured to allow traffic from the Kubernetes API into the nodes to the port 8443 even if the service is listening. See [issue on GitHub](https://github.com/istio/istio/issues/19532)
