# Hosting the relay in the cloud

The relay now lives in its own repository:

> **[github.com/langstroth-ai/langstroth-relay](https://github.com/langstroth-ai/langstroth-relay)** (MIT)

Full deploy instructions — Docker Compose, systemd, Helm, any container
platform, environment variables, TLS, token gating, Postgres, backups and
upgrades — live in that repo's **docs/deploy.md**. It's a single portable
container, so you can run it on any VM, managed container host (Render,
Railway, Fly.io, Cloud Run), or Kubernetes; the only
requirements are a public HTTPS URL and (for the snapshot store) a small volume.

Quick start:

```bash
docker build -t langstroth-relay https://github.com/langstroth-ai/langstroth-relay.git
docker run -d -p 8443:8443 -v ligus-data:/data -e LANGSTROTH_RELAY_DATA_DIR=/data langstroth-relay
curl http://localhost:8443/v1/health   # → ok
```

Then point each device at the resulting HTTPS URL (Settings → **Multiuser sync**,
or `LIGUS_RELAY_URL`). Always set a shared `LIGUS_WORKSPACE_KEY` so traffic is
end-to-end encrypted (the relay only ever sees ciphertext).

> This monorepo keeps a Rust reference relay (`crates/langstroth-relay`) used only as an
> in-process fixture for the client's tests — it is **not** the deploy target. Use
> the relay repo above for anything you run in production.
