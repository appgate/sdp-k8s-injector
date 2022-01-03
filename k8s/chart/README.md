# SDP Injector Helm Chart

## Quickstart
```
helm install sdp-k8s-client ghcr.io/appgate/sdp-k8s-client:latest
```

## Parameters
This table below was generated using [readme-generator-for-helm](https://github.com/bitnami-labs/readme-generator-for-helm)
### SDP parameters

| Name                                   | Description                                                                             | Value                                      |
| -------------------------------------- | --------------------------------------------------------------------------------------- | ------------------------------------------ |
| `sdp.image.repository`                 | SDP injector, SDP Headless Driver, SDP Headless Service, and SDP Dnsmasq image registry | `ghcr.io/appgate/sdp-k8s-client`           |
| `sdp.image.pullSecrets`                | SDP injector, SDP Headless Driver, SDP Headless Service, and SDP Dnsmasq pull secret    | `[]`                                       |
| `sdp.injector.logLevel`                | SDP Injector log level                                                                  | `info`                                     |
| `sdp.injector.image.tag`               | SDP Injector image tag                                                                  | `bf29001f31e85fe20b78ecb7ff34ecd4c67c4bbd` |
| `sdp.injector.image.pullPolicy`        | SDP Injector pull policy                                                                | `IfNotPresent`                             |
| `sdp.headlessService.image.tag`        | SDP Headless Service image tag                                                          | `latest`                                   |
| `sdp.headlessService.image.pullPolicy` | SDP Headless Service image pull policy                                                  | `Always`                                   |
| `sdp.headlessDriver.image.tag`         | SDP Headless Driver image tag                                                           | `latest`                                   |
| `sdp.headlessDriver.image.pullPolicy`  | SDP Headless Driver image pull policy                                                   | `Always`                                   |
| `sdp.dnsmasq.image.tag`                | SDP Dnsmasq image tag                                                                   | `bf29001f31e85fe20b78ecb7ff34ecd4c67c4bbd` |
| `sdp.dnsmasq.image.pullPolicy`         | SDP Dnsmasq image pull policy                                                           | `IfNotPresent`                             |


### Kubernetes parameters

| Name                    | Description                                          | Value       |
| ----------------------- | ---------------------------------------------------- | ----------- |
| `serviceAccount.create` | Enable the creation of a ServiceAccount for SDP pods | `true`      |
| `rbac.create`           | Whether to create & use RBAC resources or not        | `true`      |
| `service.type`          | Type of the service                                  | `ClusterIP` |
| `service.port`          | Port of the service                                  | `443`       |
| `replicaCount`          | Number of Kubewatch replicas to deploy               | `1`         |

