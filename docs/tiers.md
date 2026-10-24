---
title: Official and community recipes
description: "What the official and community recipe tiers mean and how they're reviewed."
---

- **Official** recipes (`recipes/official/`) are vetted and/or maintained by
  Fyra Labs. Dashboards install them as tetra's `official` catalog.
- **Community** recipes (`recipes/community/`) are contributed by third
  parties. Reviewers check that the recipe works, follows the
  [rules](./authoring.md#what-check-enforces),
  pins images from a trustworthy source, and doesn't ask for more access than
  it needs, but Fyra Labs doesn't maintain them. They're installed as
  tetra's `community` catalog.

Both tiers can use `shared/` and go through the same checks. A recipe id can
only be used once across both.
