locals {
  name            = "ccpr-${var.environment}"
  stage_url       = "https://${aws_api_gateway_rest_api.registry.id}.execute-api.${var.region}.amazonaws.com/${var.environment}" # stage_name = var.environment (avoids a dependency cycle)
  public_base_url = var.public_base_url != "" ? var.public_base_url : (var.custom_domain_name != "" ? "https://${var.custom_domain_name}" : local.stage_url)
}

data "aws_iam_policy_document" "lambda_trust" {
  statement {
    actions = ["sts:AssumeRole"]
    principals {
      type        = "Service"
      identifiers = ["lambda.amazonaws.com"]
    }
  }
}

# The execution role is also the registry's AWS principal for sts:AssumeRole into
# user-owned S3 read roles. Bucket owners trust this role ARN + their ExternalId.
resource "aws_iam_role" "lambda" {
  name               = "${local.name}-registry-lambda"
  assume_role_policy = data.aws_iam_policy_document.lambda_trust.json
}

data "aws_iam_policy_document" "lambda" {
  statement {
    sid       = "Logs"
    actions   = ["logs:CreateLogStream", "logs:PutLogEvents"]
    resources = ["${aws_cloudwatch_log_group.lambda.arn}:*"]
  }
  statement {
    sid = "Table"
    actions = [
      "dynamodb:GetItem", "dynamodb:PutItem", "dynamodb:UpdateItem", "dynamodb:DeleteItem",
      "dynamodb:Query", "dynamodb:BatchGetItem", "dynamodb:BatchWriteItem", "dynamodb:ConditionCheckItem",
    ]
    resources = [aws_dynamodb_table.registry.arn, "${aws_dynamodb_table.registry.arn}/index/*"]
  }
  statement {
    sid       = "Secrets"
    actions   = ["secretsmanager:GetSecretValue"]
    resources = [aws_secretsmanager_secret.registry.arn]
  }
  statement {
    sid       = "SecretsKms"
    actions   = ["kms:Decrypt"]
    resources = [aws_kms_key.registry.arn]
  }
  statement {
    sid       = "AssumeUserOwnedReadRoles"
    actions   = ["sts:AssumeRole"]
    resources = [var.assumable_role_arn_pattern]
  }
}

resource "aws_iam_role_policy" "lambda" {
  name   = "registry"
  role   = aws_iam_role.lambda.id
  policy = data.aws_iam_policy_document.lambda.json
}

resource "aws_cloudwatch_log_group" "lambda" {
  name              = "/aws/lambda/${local.name}-registry"
  retention_in_days = var.log_retention_days
}

resource "aws_lambda_function" "registry" {
  function_name                  = "${local.name}-registry"
  role                           = aws_iam_role.lambda.arn
  runtime                        = "nodejs22.x"
  architectures                  = ["arm64"]
  handler                        = "index.handler"
  filename                       = var.lambda_zip
  source_code_hash               = filebase64sha256(var.lambda_zip)
  memory_size                    = 512
  timeout                        = 15
  reserved_concurrent_executions = var.lambda_reserved_concurrency

  environment {
    variables = {
      ENVIRONMENT                = var.environment
      AUTH_MODE                  = "github"
      PUBLIC_BASE_URL            = local.public_base_url
      GITHUB_CLIENT_ID           = var.github_client_id
      TABLE_NAME                 = aws_dynamodb_table.registry.name
      SECRETS_ARN                = aws_secretsmanager_secret.registry.arn
      REGISTRY_AWS_PRINCIPAL_ARN = aws_iam_role.lambda.arn
      AWS_STS_REGION             = var.region
      AUDIT_RETENTION_DAYS       = tostring(var.audit_retention_days)
      NODE_OPTIONS               = "--enable-source-maps"
    }
  }

  logging_config {
    log_format = "Text"
    log_group  = aws_cloudwatch_log_group.lambda.name
  }

  depends_on = [aws_iam_role_policy.lambda]
}
