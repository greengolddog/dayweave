locals {
  labels = {
    application = "dayweave"
    environment = "production"
    managed_by  = "terraform"
  }

  backup_bucket_name = "dayweave-${substr(sha256(var.project_id), 0, 12)}"
}

resource "nebius_iam_v1_service_account" "runtime" {
  name        = "dayweave-runtime"
  parent_id   = var.project_id
  description = "Identity attached to the DayWeave VM; no broad project role."
  labels      = local.labels
}

resource "nebius_iam_v1_service_account" "backup" {
  name        = "dayweave-backup"
  parent_id   = var.project_id
  description = "Identity restricted to DayWeave backup objects."
  labels      = local.labels
}

resource "nebius_iam_v1_group" "tunnel_agents" {
  name      = "dayweave-tunnel-agents"
  parent_id = var.tenant_id
  labels    = local.labels
}

resource "nebius_iam_v1_group_membership" "tunnel_agent" {
  name      = "dayweave-tunnel-agent"
  parent_id = nebius_iam_v1_group.tunnel_agents.id
  member_id = nebius_iam_v1_service_account.runtime.id
  labels    = local.labels
}

resource "nebius_iam_v1_group" "backup_writers" {
  name      = "dayweave-backup-writers"
  parent_id = var.tenant_id
  labels    = local.labels
}

resource "nebius_iam_v1_group_membership" "backup" {
  name      = "dayweave-backup"
  parent_id = nebius_iam_v1_group.backup_writers.id
  member_id = nebius_iam_v1_service_account.backup.id
  labels    = local.labels
}

resource "nebius_iam_v1_group" "attachment_writers" {
  name      = "dayweave-attachment-writers"
  parent_id = var.tenant_id
  labels    = local.labels
}

resource "nebius_iam_v1_group_membership" "attachments" {
  name      = "dayweave-runtime-attachments"
  parent_id = nebius_iam_v1_group.attachment_writers.id
  member_id = nebius_iam_v1_service_account.runtime.id
  labels    = local.labels
}

resource "nebius_tunnel_v1_tunnel" "api" {
  name        = "dayweave-api"
  title       = "DayWeave API"
  description = "Managed HTTP and SSH ingress for the private DayWeave VM."
  parent_id   = var.project_id
  labels      = local.labels
}

resource "nebius_iam_v1_access_permit" "tunnel_agent" {
  name        = "dayweave-tunnel-connect"
  parent_id   = nebius_iam_v1_group.tunnel_agents.id
  resource_id = nebius_tunnel_v1_tunnel.api.id
  role        = "applicationtunnel.agent"
  labels      = local.labels
}

resource "nebius_storage_v1_bucket" "data" {
  name                  = local.backup_bucket_name
  parent_id             = var.project_id
  default_storage_class = "STANDARD"
  force_storage_class   = true
  max_size_bytes        = 10737418240
  object_audit_logging  = "MUTATE_ONLY"
  versioning_policy     = "ENABLED"
  labels                = local.labels

  bucket_policy = {
    rules = [
      {
        group_id = nebius_iam_v1_group.backup_writers.id
        paths    = ["postgres/*"]
        roles    = ["storage.object-editor"]
      },
      {
        group_id = nebius_iam_v1_group.attachment_writers.id
        paths    = ["attachments/*"]
        roles    = ["storage.object-editor"]
      }
    ]
  }

  lifecycle_configuration = {
    rules = [
      {
        id     = "expire-encrypted-postgres-backups"
        status = "ENABLED"
        filter = {
          prefix = "postgres/"
        }
        expiration = {
          days = 7
        }
        noncurrent_version_expiration = {
          noncurrent_days = 7
        }
        abort_incomplete_multipart_upload = {
          days_after_initiation = 1
        }
      }
    ]
  }

  lifecycle {
    prevent_destroy = true
  }
}

resource "nebius_compute_v1_disk" "boot" {
  name             = "dayweave-boot"
  parent_id        = var.project_id
  type             = "NETWORK_SSD"
  size_gibibytes   = 32
  block_size_bytes = 4096
  forbid_deletion  = true
  source_image_family = {
    image_family = "ubuntu24.04-driverless"
  }
  labels = local.labels

  lifecycle {
    prevent_destroy = true
  }
}

resource "nebius_compute_v1_instance" "app" {
  name               = "dayweave"
  hostname           = "dayweave"
  parent_id          = var.project_id
  service_account_id = nebius_iam_v1_service_account.runtime.id
  stopped            = false
  recovery_policy    = "RECOVER"
  labels             = local.labels

  resources = {
    platform = "cpu-e2"
    preset   = "2vcpu-8gb"
  }

  boot_disk = {
    attach_mode = "READ_WRITE"
    device_id   = "dayweave-root"
    existing_disk = {
      id = nebius_compute_v1_disk.boot.id
    }
  }

  network_interfaces = [
    {
      name       = "eth0"
      subnet_id  = var.subnet_id
      ip_address = {}
    }
  ]

  cloud_init_user_data = templatefile("${path.module}/cloud-init.yaml.tftpl", {
    ssh_user       = var.ssh_user
    ssh_public_key = trimspace(var.ssh_public_key)
    tunnel_id      = nebius_tunnel_v1_tunnel.api.id
  })

  depends_on = [
    nebius_iam_v1_access_permit.tunnel_agent,
    nebius_iam_v1_group_membership.tunnel_agent,
  ]
}
