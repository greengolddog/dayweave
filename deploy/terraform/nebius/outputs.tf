output "instance_id" {
  description = "DayWeave VM ID."
  value       = nebius_compute_v1_instance.app.id
}

output "backup_bucket_name" {
  description = "Private bucket used for encrypted PostgreSQL backups and attachment objects."
  value       = nebius_storage_v1_bucket.data.name
}

output "backup_service_account_id" {
  description = "Service account for which an S3-compatible access key must be created out of band."
  value       = nebius_iam_v1_service_account.backup.id
}

output "runtime_service_account_id" {
  description = "Runtime identity used by the VM for tunnel metadata auth and attachment objects."
  value       = nebius_iam_v1_service_account.runtime.id
}

output "tunnel_id" {
  description = "Nebius Tunnel ID."
  value       = nebius_tunnel_v1_tunnel.api.id
}

output "tunnel_http_url" {
  description = "Public HTTPS endpoint for the web service announced by the VM tunnel agent."
  value       = "https://web-${replace(nebius_tunnel_v1_tunnel.api.id, "/^applicationtunnel-[a-z][0-9]{2}/", "")}.tunnel.applications.eu-north1.nebius.cloud"
}

output "tunnel_ssh_route" {
  description = "TLS route consumed by scripts/ssh-nebius.sh; connecting through it performs no Nebius API call."
  value       = "ssh-${replace(nebius_tunnel_v1_tunnel.api.id, "/^applicationtunnel-[a-z][0-9]{2}/", "")}.tunnel.applications.eu-north1.nebius.cloud:443"
}

output "ssh_user" {
  description = "Administrative username for SSH over the tunnel route."
  value       = var.ssh_user
}
