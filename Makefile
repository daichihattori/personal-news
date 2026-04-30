PROJECT_ID ?= $(shell gcloud config get-value project 2>/dev/null)
REGION     ?= asia-northeast1
REGISTRY   := $(REGION)-docker.pkg.dev/$(PROJECT_ID)/personal-news

.PHONY: build push deploy tf-init tf-plan tf-apply

build:
	docker build -f Dockerfile.backend  -t $(REGISTRY)/backend:latest  .
	docker build -f Dockerfile.frontend -t $(REGISTRY)/frontend:latest .

push: build
	docker push $(REGISTRY)/backend:latest
	docker push $(REGISTRY)/frontend:latest

tf-init:
	cd infra/terraform && terraform init

tf-plan:
	cd infra/terraform && terraform plan

tf-apply:
	cd infra/terraform && terraform apply

# Full deploy: build images → push → apply terraform
deploy: push tf-apply
