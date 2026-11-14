# AppArmor profile for bubblewrap (optional)

`ligus-bwrap` lets `/usr/bin/bwrap` create user namespaces on hosts where
AppArmor restricts them and no bwrap profile ships (Ubuntu 24.04), so Ligus
uses its strongest Linux sandbox tier (bubblewrap + Landlock + seccomp)
instead of the Landlock tier. Ligus never installs it; see the header of
the profile and
[the Linux sandbox docs](https://docs.ligus.ai/ops/linux-sandbox/#apparmor-profile)
for installing, checking and removing it, and the trade-offs.
