```markdown
# our-civic-atlas-backend Development Patterns

> Auto-generated skill from repository analysis

## Overview
This skill teaches the core development patterns and conventions used in the `our-civic-atlas-backend` Rust codebase. It covers file organization, code style, commit message standards, and testing patterns to help contributors write consistent, maintainable code.

## Coding Conventions

### File Naming
- **Style:** PascalCase  
  Example:  
  ```
  MyModule.rs
  UserService.rs
  ```

### Import Style
- **Style:** Relative imports  
  Example:  
  ```rust
  use crate::models::User;
  use super::helpers::format_date;
  ```

### Export Style
- **Style:** Named exports  
  Example:  
  ```rust
  pub struct User { ... }
  pub fn create_user(...) { ... }
  ```

### Commit Messages
- **Type:** Conventional commits
- **Allowed Prefixes:** `fix`
- **Format Example:**  
  ```
  fix: correct user serialization bug
  ```

## Workflows

### Code Commit Workflow
**Trigger:** When making a code change that needs to be committed  
**Command:** `/commit`

1. Make changes following the coding conventions.
2. Write a commit message using the conventional commit format, starting with the allowed prefix (e.g., `fix:`).
3. Commit your changes.
   ```sh
   git add .
   git commit -m "fix: update user validation logic"
   ```

### File Creation Workflow
**Trigger:** When adding a new module or component  
**Command:** `/create-file`

1. Name the file using PascalCase (e.g., `NewFeature.rs`).
2. Use relative imports for dependencies within the codebase.
3. Export structs, enums, or functions using named exports.

### Testing Workflow
**Trigger:** When adding or updating functionality that requires tests  
**Command:** `/test`

1. Create a test file matching the pattern `*.test.*` (e.g., `UserService.test.rs`).
2. Write tests using the Rust testing framework (e.g., `#[test]` functions).
3. Run tests to verify correctness.
   ```sh
   cargo test
   ```

## Testing Patterns

- **Test File Naming:** Use the pattern `*.test.*` (e.g., `AuthService.test.rs`).
- **Test Structure:**  
  Example:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn test_create_user() {
          // test logic here
      }
  }
  ```
- **Framework:** Standard Rust test framework (assumed).

## Commands
| Command      | Purpose                                      |
|--------------|----------------------------------------------|
| /commit      | Commit code changes using conventional commits|
| /create-file | Add a new PascalCase-named module/component  |
| /test        | Add and run tests following the test pattern |
```
