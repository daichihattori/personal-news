PROJECT_ID ?= $(shell gcloud config get-value project 2>/dev/null)
REGION     ?= asia-northeast1
REGISTRY   := $(REGION)-docker.pkg.dev/$(PROJECT_ID)/personal-news

.PHONY: build push mirror-voicevox deploy tf-init tf-plan tf-apply

build:
	docker build --platform linux/amd64 -f Dockerfile.backend  -t $(REGISTRY)/backend:latest  .
	docker build --platform linux/amd64 -f Dockerfile.frontend -t $(REGISTRY)/frontend:latest .

push: build
	docker push $(REGISTRY)/backend:latest
	docker push $(REGISTRY)/frontend:latest

# Mirror VOICEVOX from Docker Hub to Artifact Registry (run once before first deploy)
mirror-voicevox:
	docker pull --platform linux/amd64 voicevox/voicevox_engine:cpu-ubuntu20.04-latest
	docker tag voicevox/voicevox_engine:cpu-ubuntu20.04-latest $(REGISTRY)/voicevox:latest
	docker push $(REGISTRY)/voicevox:latest

tf-init:
	cd infra/terraform && terraform init

tf-plan:
	cd infra/terraform && terraform plan

tf-apply:
	cd infra/terraform && terraform apply

# Full deploy: build images → push → apply terraform
deploy: push tf-apply
