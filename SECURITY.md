# Security policy

## Reporting a problem

Please report security problems privately through
[GitHub security advisories](https://github.com/Yejashi/nora/security/advisories/new),
not in a public issue. Include the nora version or commit, and the steps
or bytes that trigger the problem. You will get a reply as soon as
possible, and a fix will be released before the details are made public.

nora is pre-release software, so fixes go to the latest commit on `main`.

## What nora treats as untrusted

Anything a program running in the terminal prints is untrusted: a file
you `cat`, a web page you `curl`, or the output of a remote host over ssh.
nora limits what that output can do:

- it cannot read or write the clipboard (OSC 52 is disabled);
- control sequences, images and the queues between threads are
  size-capped, so output cannot exhaust memory;
- kitty graphics may read only regular files and POSIX shared memory,
  never anything else under `/proc`, `/sys` or `/dev`; only temporary
  files with the protocol's marker in their name are deleted;
- hyperlinks open only for `http`, `https`, `mailto`, `file` and `ftp`,
  only on Ctrl+click, through the platform opener with no shell involved;
- the working directory a program reports is used only to rank
  suggestions; a directory on another host is ignored.

Shell history used for suggestions stays on your machine, in a file only
you can read.

Problems in these protections are security problems. Please report them
as described above.
