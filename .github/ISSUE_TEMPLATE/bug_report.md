name: Bug report
description: Something in EasyNotes does not work as documented
labels: ["bug"]
body:
  - type: markdown
    attributes:
      value: |
        Thanks for the report. Please do not include note contents, recovery
        phrases, keys, invite links or other secrets. A security issue belongs
        in the private advisory flow in SECURITY.md, not here.
  - type: textarea
    id: what
    attributes:
      label: What happened
      description: What did you do, what did you expect, and what happened instead?
    validations:
      required: true
  - type: textarea
    id: steps
    attributes:
      label: Steps to reproduce
      placeholder: |
        1. ...
        2. ...
        3. ...
    validations:
      required: true
  - type: dropdown
    id: platform
    attributes:
      label: Platform
      options:
        - Android
        - Web
        - iOS
        - Linux
        - Windows
        - Relay
        - Not platform-specific
    validations:
      required: true
  - type: input
    id: version
    attributes:
      label: Version or commit
      description: The app version, or the commit you built from.
  - type: textarea
    id: logs
    attributes:
      label: Relevant logs or a failing test
      description: Remove secrets and personal data first. A failing test in `core/tests/` or `relay/tests/` is ideal.
      render: text
  - type: checkboxes
    id: checks
    attributes:
      label: Before posting
      options:
        - label: I have removed note contents, recovery phrases, keys and invite links from this report.
          required: true
        - label: This is not a security vulnerability, or I have read SECURITY.md and will use the private advisory flow.
          required: true
