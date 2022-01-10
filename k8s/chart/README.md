# SDP Injector Helm Chart

## Quickstart
```
helm install sdp-k8s-client ghcr.io/appgate/sdp-k8s-client:<version> 
```

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