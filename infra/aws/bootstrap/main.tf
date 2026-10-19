# One-time bootstrap per AWS account: Terraform state buckets and the GitHub Actions
# OIDC deploy roles (no long-lived AWS keys in GitHub). Applied manually by an
# administrator with local state; it holds no secrets. See docs/runbook.md.
terraform {
  required_version = ">= 1.10.0"
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.17"
    }
  }
}

variable "region" {
  type    = string
  default = "ap-northeast-1"
}

variable "github_repository" {
  type    = string
  default = "hakuturu583/carla_content_pack_registry"
}

variable "state_bucket_prefix" {
  description = "Globally unique prefix for the state buckets."
  type        = string
}

variable "environments" {
  type    = list(string)
  default = ["staging", "production"]
}

provider "aws" {
  region = var.region
}

resource "aws_s3_bucket" "state" {
  for_each = toset(var.environments)
  bucket   = "${var.state_bucket_prefix}-ccpr-tfstate-${each.key}"
}

resource "aws_s3_bucket_versioning" "state" {
  for_each = aws_s3_bucket.state
  bucket   = each.value.id
  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "state" {
  for_each = aws_s3_bucket.state
  bucket   = each.value.id
  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm = "aws:kms"
    }
  }
}

resource "aws_s3_bucket_public_access_block" "state" {
  for_each                = aws_s3_bucket.state
  bucket                  = each.value.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_iam_openid_connect_provider" "github" {
  url            = "https://token.actions.githubusercontent.com"
  client_id_list = ["sts.amazonaws.com"]
}

data "aws_iam_policy_document" "deploy_trust" {
  for_each = toset(var.environments)
  statement {
    actions = ["sts:AssumeRoleWithWebIdentity"]
    principals {
      type        = "Federated"
      identifiers = [aws_iam_openid_connect_provider.github.arn]
    }
    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:aud"
      values   = ["sts.amazonaws.com"]
    }
    # Only jobs bound to the matching GitHub Environment: "<env>" (approval required)
    # or "<env>-plan" (plan only). TODO: give "-plan" a separate read-only role.
    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:sub"
      values = [
        "repo:${var.github_repository}:environment:${each.key}",
        "repo:${var.github_repository}:environment:${each.key}-plan",
      ]
    }
  }
}

resource "aws_iam_role" "deploy" {
  for_each             = toset(var.environments)
  name                 = "ccpr-${each.key}-github-deploy"
  assume_role_policy   = data.aws_iam_policy_document.deploy_trust[each.key].json
  max_session_duration = 3600
}

# Broad by necessity (it creates IAM roles, WAF, API Gateway...). Tighten once the
# resource set is stable; IAM changes are limited to ccpr-* names.
resource "aws_iam_role_policy_attachment" "deploy_poweruser" {
  for_each   = aws_iam_role.deploy
  role       = each.value.name
  policy_arn = "arn:aws:iam::aws:policy/PowerUserAccess"
}

data "aws_iam_policy_document" "deploy_iam" {
  statement {
    actions = [
      "iam:CreateRole", "iam:DeleteRole", "iam:GetRole", "iam:UpdateRole", "iam:TagRole", "iam:UntagRole",
      "iam:PutRolePolicy", "iam:DeleteRolePolicy", "iam:GetRolePolicy", "iam:ListRolePolicies",
      "iam:ListAttachedRolePolicies", "iam:ListInstanceProfilesForRole", "iam:PassRole", "iam:UpdateAssumeRolePolicy",
      "iam:CreateUser", "iam:DeleteUser", "iam:GetUser", "iam:TagUser", "iam:PutUserPolicy", "iam:DeleteUserPolicy",
      "iam:GetUserPolicy", "iam:ListUserPolicies", "iam:ListAttachedUserPolicies", "iam:ListAccessKeys",
      "iam:ListGroupsForUser",
    ]
    resources = ["arn:aws:iam::*:role/ccpr-*", "arn:aws:iam::*:user/ccpr-*"]
  }
}

resource "aws_iam_role_policy" "deploy_iam" {
  for_each = aws_iam_role.deploy
  name     = "ccpr-iam"
  role     = each.value.name
  policy   = data.aws_iam_policy_document.deploy_iam.json
}

output "deploy_role_arns" {
  value = { for k, r in aws_iam_role.deploy : k => r.arn }
}

output "state_buckets" {
  value = { for k, b in aws_s3_bucket.state : k => b.bucket }
}
