# Security policy

Please report vulnerabilities privately through a GitHub security advisory ("Report a vulnerability" under the
repository's Security tab), not in public issues.

Include the affected crate and version, how to reproduce, and the impact you expect. You should get a reply within a
week. Fixes ship in a patch release of the latest version, and the advisory is published once a fix is available.

`prequal-epp` and `prequal-router` sit on the request path in front of model servers, so issues that let a client
crash them, exhaust their memory, or steer traffic to an unintended backend are in scope.
