variable "nebius_profile" {
  description = "User-authorized Nebius CLI profile used only to plan and bootstrap least-privilege service accounts."
  type        = string
  default     = "lol"

  validation {
    condition     = var.nebius_profile == "lol"
    error_message = "nebius_profile must be the explicitly authorized lol profile."
  }
}

variable "project_id" {
  description = "Existing eu-north1 Nebius project ID. Keep this in ignored local.auto.tfvars.json."
  type        = string

  validation {
    condition     = startswith(var.project_id, "project-")
    error_message = "project_id must be a Nebius project ID."
  }
}

variable "tenant_id" {
  description = "Tenant containing the project. Keep this in ignored local.auto.tfvars.json."
  type        = string

  validation {
    condition     = startswith(var.tenant_id, "tenant-")
    error_message = "tenant_id must be a Nebius tenant ID."
  }
}

variable "subnet_id" {
  description = "Existing private subnet in the project. Keep this in ignored local.auto.tfvars.json."
  type        = string

  validation {
    condition     = startswith(var.subnet_id, "vpcsubnet-")
    error_message = "subnet_id must be a Nebius subnet ID."
  }
}

variable "ssh_user" {
  description = "Administrative Linux user created by cloud-init."
  type        = string
  default     = "dayweave"

  validation {
    condition = (
      can(regex("^[a-z_][a-z0-9_-]{0,30}$", var.ssh_user)) &&
      !contains([
        "_apt", "admin", "backup", "bin", "daemon", "games", "gnats",
        "irc", "landscape", "list", "lp", "mail", "man", "messagebus",
        "news", "nobody", "pollinate", "proxy", "root", "sshd", "sync",
        "sys", "syslog", "systemd-network", "systemd-timesync", "tcpdump",
        "tss", "ubuntu", "uucp", "uuidd", "www-data",
      ], lower(var.ssh_user))
    )
    error_message = "ssh_user must be a valid non-reserved Linux account name."
  }
}

variable "ssh_public_key" {
  description = "Public SSH key installed by cloud-init. The private key never enters Terraform."
  type        = string

  validation {
    condition = (
      !can(regex("[\\r\\n]", trimspace(var.ssh_public_key))) &&
      can(regex(
        "^(ssh-ed25519|ssh-rsa|ecdsa-sha2-nistp256) [A-Za-z0-9+/]+={0,3}( [^\\r\\n]+)?$",
        trimspace(var.ssh_public_key)
      ))
    )
    error_message = "ssh_public_key must be one valid single-line OpenSSH Ed25519, RSA, or P-256 public key."
  }
}
