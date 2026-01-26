.PHONY: bump-major bump-minor bump-patch

# Get current version from package.json
CURRENT_VERSION := $(shell jq -r '.version' package.json)

# Parse version components
MAJOR := $(shell echo $(CURRENT_VERSION) | cut -d. -f1)
MINOR := $(shell echo $(CURRENT_VERSION) | cut -d. -f2)
PATCH := $(shell echo $(CURRENT_VERSION) | cut -d. -f3)

bump-major:
	@echo "Bumping major version from $(CURRENT_VERSION)"
	$(eval NEW_VERSION := $(shell echo $$(($(MAJOR) + 1)).0.0))
	@$(MAKE) update-version NEW_VERSION=$(NEW_VERSION)

bump-minor:
	@echo "Bumping minor version from $(CURRENT_VERSION)"
	$(eval NEW_VERSION := $(MAJOR).$(shell echo $$(($(MINOR) + 1))).0)
	@$(MAKE) update-version NEW_VERSION=$(NEW_VERSION)

bump-patch:
	@echo "Bumping patch version from $(CURRENT_VERSION)"
	$(eval NEW_VERSION := $(MAJOR).$(MINOR).$(shell echo $$(($(PATCH) + 1))))
	@$(MAKE) update-version NEW_VERSION=$(NEW_VERSION)

update-version:
	@echo "Updating version to $(NEW_VERSION)"
	@jq '.version = "$(NEW_VERSION)"' package.json > package.json.tmp && mv package.json.tmp package.json
	@sed -i '' 's/^version = ".*"/version = "$(NEW_VERSION)"/' src-tauri/Cargo.toml
	@git add package.json src-tauri/Cargo.toml
	@git commit -m "chore: bump version to $(NEW_VERSION)"
	@echo "Version bumped to $(NEW_VERSION) and committed"
	@echo "Run 'git push origin main' to trigger the release workflow"
