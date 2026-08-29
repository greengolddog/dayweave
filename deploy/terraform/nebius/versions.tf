terraform {
  required_version = ">= 1.10.0, < 2.0.0"

  required_providers {
    nebius = {
      source  = "nebius/nebius"
      version = "= 0.6.48"
    }
  }
}

provider "nebius" {
  profile = {
    name            = var.nebius_profile
    no_browser_open = true
  }
}
