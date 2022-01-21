# SDP Kubernetes Client
The sdp-k8s-client integrates the SDP client into Kubernetes clusters.

By injecting an SDP Client into pods on-demand, any Kubernetes workloads can access resources behind an SDP system and be managed in a uniform fashion.

## Requirements
The following tools are required to install the SDP Kubernetes Client:
* helm v3.7.0+ - https://helm.sh/docs/intro/install/
* kubectl - https://kubernetes.io/docs/tasks/tools/#kubectl

## Getting Started
1. Install the SDP Kubernetes Client with Helm 
    ```bash
    $ export HELM_EXPERIENTAL_OCI=1
    $ helm install sdp-k8s-client oci://ghcr.io/appgate/sdp-k8s-client --version <VERSION>
    ```
    Browse the available versions on [Appgate GitHub Container Registry](https://github.com/appgate/sdp-k8s-client/pkgs/container/charts%2Fsdp-k8s-client)


2. Create a `sdp-demo` namespace and label the namespace with `sdp-injection="enabled"` for SDP client injection
    ```bash
    $ kubectl create namespace sdp-demo
    $ kubectl label namespace sdp-demo --overwrite sdp-injection="enabled"
    ```


3. Create a secret containing username, password, and profile URL for default authentication
    ```bash
    $ kubectl create secret generic sdp-injector-client-secrets \
       --namespace sdp-demo \
       --from-literal=client-username="<USERNAME>" \
       --from-literal=client-password="<PASSWORD>" \
       --from-literal=client-controller-url="<PROFILE_URL>"
    ```


4. Create a configmap containing values for default configuration
    ```bash
    $ kubectl create configmap sdp-injector-client-config \
       --namespace sdp-demo \
       --from-literal=client-log-level="<LOG_LEVEL>"
    ```

    The following configurations are supported:

| Name               | Description                                                                                               | Example                                |
|--------------------|-----------------------------------------------------------------------------------------------------------|----------------------------------------|
| `client-log-level` | The log level of the client                                                                               | `Info` `Debug`                         |
| `client-device-id` | The device ID to use for the client in UUID v4 format. If empty, the injector will generate a random UUID | `860ab4cc-50f4-4c18-9e9c-1709d5419f1d` |


5. Test the deployment by creating a busybox pod to ping a resource behind an SDP system
    ```bash
    $ kubectl run --namespace sdp-demo -i --tty busybox --image=busybox -- sh
    $ /# ping <IP_ADDRESS>
    ```

## Advanced Usage
### Namespace Labels
SDP injection is bound to namespaces. Adding the label `sdp-injection="enabled"` to a namespace will instruct the injector to inject a client to all pods in the namespace.
```bash
$ kubectl label --overwrite namespace sdp-demo sdp-injection="enabled"
```
Check the label of the namespace to see if injection is enabled
```bash
$ kubectl get namespace sdp-demo -L sdp-injection
NAME          STATUS   AGE    SDP-INJECTION
sdp-demo      Active   1m     enabled
```

### Excluding Pods from Injection
To prevent client injection at a per-Pod basis, annotate the pod with `sdp-injector="false"`. Any pod with this annotation will not get a SDP client even if it exists inside a namespace label with `sdp-injection="enabled"`
```bash
$ kubectl annotate --overwrite pod <POD> sdp-injector="false"
```

### Using Non-Default Secret or ConfigMap
By default, the SDP injector will look for configmap `sdp-injector-client-config` for configuration and secret `sdp-injector-client-secrets` for authentication.

To use a non-default configuration/authentication at a per-Pod basis, annotate the pod with `sdp-injector-client-config="<CONFIGMAP>"` or `sdp-injector-client-secrets="<SECRET>"`. This annotation will instruct the injector to use this configmap/secret instead of the default.

Use non-default configuration
```bash
$ kubectl annotate --overwrite pod <POD> sdp-injector-client-config="<CONFIGMAP>"
```

Use non-default authentication credentials
```bash
$ kubectl annotate --overwrite pod <POD> sdp-injector-client-secrets="<SECRET>"
```

## How It Works
### sdp-dnsmasq
The SDP client can query specific hosts to specific DNS server behind the SDP system because the sdp-driver notifies the domains and DNS behind an SDP system.

In the SDP Kubernetes Client, the dnsmasq instance is configured according to the events that the sdp-driver sends. Since only sdp-driver container can modify `/etc/resolv.conf`, the setup is done in the following steps:

1. The sdp-dnsmasq container grabs the address of the kube-dns service and starts a new dnsmasq instance using that DNS server as upstream. This allows the dnsmasq instance to forward everything into the kube-dns service. 
2. The sdp-dnsmasq container  opens a UNIX socket to receive events from the sdp-driver container when there are changes in the DNS settings.
3. The sdp-driver container waits for the service to connect. Once connected, it calls the [sdp-driver-set-dns](./assets/sdp-driver-set-dns) script.
4. [sdp-driver-set-dns](./assets/sdp-driver-set-dns) configures `/etc/resolv.conf` to point to sdp-dnsmasq. From this point onwards, sdp-dnsmasq takes the responsibility of resolving names inside the pod. 
5. An event is sent to sdp-dnsmasq via UNIX socket by the sdp-driver. Then the sdp-dnsmasq configures the dnsmasq according to it.

## Known Issues
### Google Kubernetes Engine (GKE)
When running on GKE, the firewall needs to be configured to allow traffic from the Kubernets API into the nodes to the port 8443 even if the service is listening. See [issue on GitHub](https://github.com/istio/istio/issues/19532)

## Parameters
### SDP parameters
| Name                                   | Description                                                                              | Value                            |
| -------------------------------------- | ---------------------------------------------------------------------------------------- | -------------------------------- |
| `global.image.repository`              | Image registry to use for all SDP images.                                                | `ghcr.io/appgate/sdp-k8s-client` |
| `global.image.tag`                     | Image tag to use for all SDP images. If not set, it defaults to `.Chart.appVersion`.     | `""`                             |
| `global.image.pullPolicy`              | Image pull policy to use for all SDP images.                                             | `IfNotPresent`                   |
| `global.image.pullSecrets`             | Image pull secret to use for all SDP images.                                             | `[]`                             |
| `sdp.injector.logLevel`                | SDP Injector log level.                                                                  | `info`                           |
| `sdp.injector.image.repository`        | SDP Injector image repository. If set, it overrides `.global.image.repository`.          | `""`                             |
| `sdp.injector.image.tag`               | SDP Injector image tag. If set, it overrides `.global.image.tag`.                        | `""`                             |
| `sdp.injector.image.pullPolicy`        | SDP Injector pull policy. If set, it overrides `.global.image.pullPolicy`.               | `""`                             |
| `sdp.headlessService.image.tag`        | SDP Headless Service image repository. If set, it overrides `.global.image.repository`.  | `""`                             |
| `sdp.headlessService.image.repository` | SDP Headless Service image tag. If set, it overrides `.global.image.tag`.                | `""`                             |
| `sdp.headlessService.image.pullPolicy` | SDP Headless Service image pull policy. If set, it overrides `.global.image.pullPolicy`. | `""`                             |
| `sdp.headlessDriver.image.repository`  | SDP Headless Driver image repository. If set, it overrides `.global.image.repository`.   | `""`                             |
| `sdp.headlessDriver.image.tag`         | SDP Headless Driver image tag. If set, it overrides `.global.image.tag`.                 | `""`                             |
| `sdp.headlessDriver.image.pullPolicy`  | SDP Headless Service image pull policy. If set, it overrides `.global.image.pullPolicy`. | `""`                             |
| `sdp.dnsmasq.image.repository`         | SDP Dnsmasq image repository. If set, it overrides `.global.image.repository`.           | `""`                             |
| `sdp.dnsmasq.image.tag`                | SDP Dnsmasq image tag. If set, it overrides `.global.image.tag`.                         | `""`                             |
| `sdp.dnsmasq.image.pullPolicy`         | SDP Dnsmasq image pull policy. If set, it overrides `.global.image.pullPolicy`.          | `""`                             |

### Kubernetes parameters
| Name                    | Description                                          | Value       |
| ----------------------- | ---------------------------------------------------- | ----------- |
| `serviceAccount.create` | Enable the creation of a ServiceAccount for SDP pods | `true`      |
| `rbac.create`           | Whether to create & use RBAC resources or not        | `true`      |
| `service.type`          | Type of the service                                  | `ClusterIP` |
| `service.port`          | Port of the service                                  | `443`       |
| `replicaCount`          | Number of SDP client replicas to deploy              | `1`         |

This table above was generated using [readme-generator-for-helm](https://github.com/bitnami-labs/readme-generator-for-helm)
