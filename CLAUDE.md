# Project Rules

## Git Workflow
- At the start of each conversation, propose creating a new Git branch for the upcoming changes.
- When the task is complete, propose to commit the changes and merge the branch into main.

## Tests
- Always create new tests or update existing ones to cover the changes made.
- Tests must validate the expected behavior independently of the implementation. Do not write tests that simply mirror the code you wrote — tests should verify correctness from the user's perspective, not confirm that your implementation runs without error.
- **CRITICAL: Show test output before validation.** Every test MUST include `println!`/`eprintln!` statements that display key values (API responses, computed results, state changes). After writing a test, run it with `cargo test <test_name> -- --nocapture` and show the full output to the user. The user validates the test based on the printed output, NOT just on whether it passes. A test that passes but produces wrong output is a bug.
- Never remove debug prints from tests after validation — they serve as living documentation and help catch regressions.

## Code Quality
- Never use placeholder code, TODO stubs, or incomplete implementations. Always write the full, working code immediately.

## GitHub Integration and Version Control

**CRITICAL: All projects must use Git and GitHub for version control.**

### Initial Setup
- Initialize Git repository for ALL new projects immediately.
- Create `.gitignore` with common exclusions (`node_modules`, `__pycache__`, `.venv`, `.env`, etc.).
- NEVER commit secrets, API keys, or credentials.
- Always include `.env.example` for required environment variables.

### Branching Strategy

For solo projects:
- `main` branch for stable code.
- Feature branches: `feature/add-user-auth`
- Fix branches: `fix/memory-leak`
- Merge back to `main` when complete.

For collaborative projects:
- `main` — production-ready code.
- `develop` — integration branch.
- `feature/*` — new features.
- `hotfix/*` — urgent production fixes.

### Commit Best Practices
- Atomic commits (one logical change per commit).
- Clear messages: `"Add user authentication with JWT"`
- Include context: `"Fixes rate limiting issue causing 429 errors"`
- Reference issues when applicable: `"Closes #42"`
- Commit message format: brief description + context (1-2 sentences).

### When to Commit
- After completing a discrete feature/fix.
- Before risky refactoring (commit working state).
- After CodeRabbit review and fixes.
- Before ending work session.

### Push Frequency
- Make frequent, meaningful commits with clear messages.
- Push to GitHub regularly to maintain backup.
- After every completed and tested feature.
- At least once per work session.
- Before deployment.

### GitHub Operations
- Use `gh` CLI for GitHub operations when possible.
- Create branches for major features/experiments.
- Use GitHub Issues for tracking bugs and feature requests.

## Update PROJECT_LOG.md with Rebuild-Level Detail

**CRITICAL: `PROJECT_LOG.md` must contain sufficient detail to rebuild the entire project from the markdown alone.**

### Project Type Templates

**For Web Applications (Frontend/Backend):**
- Tech stack (framework, runtime versions).
- API endpoints with request/response schemas.
- Database schema and migrations.
- Authentication/authorization setup.
- Environment variables (`.env.example`).
- Deployment workflow (CI/CD, hosting platform).

**For CLI Tools:**
- Installation methods (`pip`, `cargo`, `go install`).
- Command-line arguments and flags.
- Configuration file formats.
- Build instructions for binaries.

**For Data Science/ML Projects:**
- Dataset sources and preprocessing steps.
- Model architecture and hyperparameters.
- Training pipeline (scripts, hardware requirements).
- Inference/deployment methods.
- Dependencies (CUDA, specific library versions).

**For Infrastructure/DevOps:**
- Terraform/CloudFormation configurations.
- Service topology diagrams.
- Secrets management approach.
- Monitoring and alerting setup.

**For Libraries/Packages:**
- Public API documentation.
- Installation from source.
- Testing procedures.
- Publishing workflow (npm, PyPI, crates.io).

### Minimum Required Sections

1. **Project Overview**
   - Business/app name and description.
   - Domain, hosting, repository URLs.
   - Tech stack summary.
   - Key stakeholder info (emails, accounts).

2. **Version History**
   - Date, version number (semantic versioning).
   - What was accomplished.
   - File count and total size.
   - Deployment status.

3. **Complete Project Structure**
   - Full directory tree with comments.
   - Line counts for each file.
   - Purpose of each file/directory.

4. **Technical Architecture**
   - Frontend/backend stack details.
   - Key features with implementation specifics.
   - Configuration details (build settings, environment vars).
   - API endpoints, database schema, or core functionality.

5. **File Contents Summary**
   - For config files: include actual content or detailed breakdown.
   - For code files: list key functions/components with line numbers.
   - For HTML/templates: list all sections and their purposes.

6. **Deployment Workflow**
   - Local development steps.
   - Build/test commands.
   - Deployment process (CI/CD, manual steps).
   - Environment setup requirements.

7. **Domain & Infrastructure Setup**
   - DNS configuration.
   - Hosting platform settings.
   - Third-party integrations (analytics, forms, etc.).

8. **Complete Rebuild Instructions**
   - Step-by-step guide to recreate project from scratch.
   - All commands needed (with explanations).
   - All file creation steps.
   - All configuration steps.
   - Deployment and domain setup.

9. **Contact/Business Information**
   - Phone numbers, addresses, key contacts.
   - Service details, licenses, certifications.

10. **Maintenance Notes**
    - Known issues or future enhancements.
    - Update procedures.
    - Monitoring and analytics.

### When to Update PROJECT_LOG.md
- After completing major phases or milestones.
- When adding new features or components.
- After significant refactoring.
- When deployment configuration changes.
- Before ending a work session.

**Quality Standard:** Someone with basic technical knowledge should be able to rebuild the ENTIRE project (minus proprietary assets like images) using ONLY the `PROJECT_LOG.md` file.
