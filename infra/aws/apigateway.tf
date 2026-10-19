resource "aws_api_gateway_rest_api" "registry" {
  name                         = "${local.name}-registry"
  description                  = "CARLA Content Pack Registry (${var.environment})"
  disable_execute_api_endpoint = var.custom_domain_name != ""

  endpoint_configuration {
    types = ["REGIONAL"]
  }
}

resource "aws_api_gateway_resource" "proxy" {
  rest_api_id = aws_api_gateway_rest_api.registry.id
  parent_id   = aws_api_gateway_rest_api.registry.root_resource_id
  path_part   = "{proxy+}"
}

resource "aws_api_gateway_method" "root" {
  rest_api_id   = aws_api_gateway_rest_api.registry.id
  resource_id   = aws_api_gateway_rest_api.registry.root_resource_id
  http_method   = "ANY"
  authorization = "NONE" # authentication is done by the application (GitHub OAuth)
}

resource "aws_api_gateway_method" "proxy" {
  rest_api_id   = aws_api_gateway_rest_api.registry.id
  resource_id   = aws_api_gateway_resource.proxy.id
  http_method   = "ANY"
  authorization = "NONE"
}

resource "aws_api_gateway_integration" "root" {
  rest_api_id             = aws_api_gateway_rest_api.registry.id
  resource_id             = aws_api_gateway_rest_api.registry.root_resource_id
  http_method             = aws_api_gateway_method.root.http_method
  type                    = "AWS_PROXY"
  integration_http_method = "POST"
  uri                     = aws_lambda_function.registry.invoke_arn
}

resource "aws_api_gateway_integration" "proxy" {
  rest_api_id             = aws_api_gateway_rest_api.registry.id
  resource_id             = aws_api_gateway_resource.proxy.id
  http_method             = aws_api_gateway_method.proxy.http_method
  type                    = "AWS_PROXY"
  integration_http_method = "POST"
  uri                     = aws_lambda_function.registry.invoke_arn
}

resource "aws_api_gateway_deployment" "registry" {
  rest_api_id = aws_api_gateway_rest_api.registry.id

  triggers = {
    redeploy = sha1(jsonencode([
      aws_api_gateway_resource.proxy.id,
      aws_api_gateway_method.root.id,
      aws_api_gateway_method.proxy.id,
      aws_api_gateway_integration.root.id,
      aws_api_gateway_integration.proxy.id,
    ]))
  }

  lifecycle {
    create_before_destroy = true
  }
}

resource "aws_cloudwatch_log_group" "api_access" {
  name              = "/aws/apigateway/${local.name}-registry/access"
  retention_in_days = var.log_retention_days
}

resource "aws_api_gateway_stage" "registry" {
  rest_api_id   = aws_api_gateway_rest_api.registry.id
  deployment_id = aws_api_gateway_deployment.registry.id
  stage_name    = var.environment

  access_log_settings {
    destination_arn = aws_cloudwatch_log_group.api_access.arn
    format = jsonencode({
      requestId = "$context.requestId", ip = "$context.identity.sourceIp", method = "$context.httpMethod",
      path      = "$context.resourcePath", status = "$context.status", latencyMs = "$context.responseLatency",
      wafStatus = "$context.waf.status", userAgent = "$context.identity.userAgent"
    })
  }
}

resource "aws_api_gateway_method_settings" "all" {
  rest_api_id = aws_api_gateway_rest_api.registry.id
  stage_name  = aws_api_gateway_stage.registry.stage_name
  method_path = "*/*"

  settings {
    metrics_enabled        = true
    throttling_rate_limit  = var.api_throttle_rate
    throttling_burst_limit = var.api_throttle_burst
  }
}

resource "aws_lambda_permission" "apigw" {
  statement_id  = "AllowApiGatewayInvoke"
  action        = "lambda:InvokeFunction"
  function_name = aws_lambda_function.registry.function_name
  principal     = "apigateway.amazonaws.com"
  source_arn    = "${aws_api_gateway_rest_api.registry.execution_arn}/*/*"
}

# --- Optional custom domain --------------------------------------------------------

resource "aws_api_gateway_domain_name" "registry" {
  count                    = var.custom_domain_name != "" ? 1 : 0
  domain_name              = var.custom_domain_name
  regional_certificate_arn = var.certificate_arn
  security_policy          = "TLS_1_2"

  endpoint_configuration {
    types = ["REGIONAL"]
  }
}

resource "aws_api_gateway_base_path_mapping" "registry" {
  count       = var.custom_domain_name != "" ? 1 : 0
  api_id      = aws_api_gateway_rest_api.registry.id
  stage_name  = aws_api_gateway_stage.registry.stage_name
  domain_name = aws_api_gateway_domain_name.registry[0].domain_name
}
