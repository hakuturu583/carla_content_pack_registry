terraform {
  required_version = ">= 1.10.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.17"
    }
  }

  # Per-environment state: terraform init -backend-config=envs/<env>.backend.hcl
  backend "s3" {}
}

provider "aws" {
  region = var.region
  default_tags {
    tags = {
      app         = "carla-content-pack-registry"
      environment = var.environment
      managed_by  = "terraform"
    }
  }
}
