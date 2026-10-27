name: Feature request
description: Suggest a change to how EasyNotes works
labels: ["enhancement"]
body:
  - type: markdown
    attributes:
      value: |
        EasyNotes is intentionally narrow: a Keep-shaped shared list, encrypted
        by default. Please describe the problem before the solution.
  - type: textarea
    id: problem
    attributes:
      label: The problem
      description: What are you trying to do that EasyNotes makes hard or impossible?
    validations:
      required: true
  - type: textarea
    id: idea
    attributes:
      label: What you would like
      description: A sketch is enough. If it changes the protocol or stored data, say so.
  - type: textarea
    id: alternatives
    attributes:
      label: Alternatives considered
      description: Other apps, workarounds, or a smaller version of the same change.
  - type: checkboxes
    id: scope
    attributes:
      label: Scope
      options:
        - label: This does not require a central service that can read note content.
          required: true
