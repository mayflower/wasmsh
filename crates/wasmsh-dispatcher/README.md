# wasmsh-dispatcher

Part of the [wasmsh](https://github.com/mayflower/wasmsh) workspace — a browser-first shell runtime in Rust.

Axum HTTP control plane for the scalable deployment path. Routes session requests across a pool of `wasmsh-runner` pods with session affinity and restore-capacity-aware scheduling. Shipped as the `ghcr.io/mayflower/wasmsh-dispatcher` container image and installed via the [Helm chart](https://github.com/mayflower/wasmsh/tree/main/deploy/helm/wasmsh).

See the [main repository](https://github.com/mayflower/wasmsh) for documentation, architecture overview, and usage examples. The HTTP contract is documented at [`docs/reference/dispatcher-api.md`](https://github.com/mayflower/wasmsh/blob/main/docs/reference/dispatcher-api.md); the scalable architecture rationale at [`docs/explanation/snapshot-runner.md`](https://github.com/mayflower/wasmsh/blob/main/docs/explanation/snapshot-runner.md).
