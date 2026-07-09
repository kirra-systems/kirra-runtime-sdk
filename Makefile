# Kirra — top-level convenience targets.
#
# EP-18: `make safety-case` assembles the versioned, hash-chained evidence
# bundle (reviewed manifests + safety-case docs + gates re-executed at bundle
# time + referenced CI lanes) into target/safety-case/, then SELF-VERIFIES it
# (re-hash every element, re-walk the chain, recompute the bundle digest) —
# a bundle that does not verify never ships. Run by the release workflow on
# every tag; see ci/build_safety_case.py for the element inventory.

.PHONY: safety-case
safety-case:
	python3 ci/build_safety_case.py --out target/safety-case
	python3 ci/build_safety_case.py --verify target/safety-case
