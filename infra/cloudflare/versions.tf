terraform {
  required_version = ">= 1.10.0"

  required_providers {
    cloudflare = {
      source  = "cloudflare/cloudflare"
      version = "~> 5.11"
    }
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.17"
    }
  }

  # Per-environment state: terraform init -backend-config=envs/<env>.backend.hcl
  # Staging and production use different state keys (and should use different buckets).
  backend "s3" {}
}

provider "cloudflare" {
  # API token from CLOUDFLARE_API_TOKEN (scoped to this account/zone: D1 edit, Zone WAF edit, DNS edit).
}

provider "aws" {
  region = var.aws_region
  # Only used when enable_aws_sts_principal = true (Cloudflare-hosted registry + AWS S3 buckets).
  skip_credentials_validation = !var.enable_aws_sts_principal
  skip_requesting_account_id  = !var.enable_aws_sts_principal
  skip_metadata_api_check     = true
}
