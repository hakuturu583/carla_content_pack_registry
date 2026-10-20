# Secret container only. The value ({"GITHUB_CLIENT_SECRET": "...", "CREDENTIAL_KEYS": "..."})
# is written out-of-band (docs/runbook.md) so it never appears in Terraform state or plans.
resource "aws_kms_key" "registry" {
  description             = "${local.name} registry secrets"
  enable_key_rotation     = true
  deletion_window_in_days = 30
}

resource "aws_kms_alias" "registry" {
  name          = "alias/${local.name}-registry"
  target_key_id = aws_kms_key.registry.key_id
}

resource "aws_secretsmanager_secret" "registry" {
  name                    = "${local.name}/registry"
  description             = "GITHUB_CLIENT_SECRET and CREDENTIAL_KEYS for the registry Lambda"
  kms_key_id              = aws_kms_key.registry.arn
  recovery_window_in_days = 30
}
