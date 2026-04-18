terraform {
  required_version = ">= 1.5"
}

provider "aws" {
  region = "ap-northeast-2"
}

# Newer addition not covered by the original workspace list — the
# reconfigure agent should add a workspace for this directory.
