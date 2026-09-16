# Worker resource limits before OCR integration

The CPU probe establishes neither an integrated acquisition provider nor portable
process isolation. The CLI previously rejected every non-Linux worker before
reading input. macOS now uses the existing Rust resource-limit calls: 2 GiB of
address space, the caller's CPU deadline, and no core dumps. Any failed limit
installation still returns unsupported before input processing. Linux limits and
worker framing are unchanged; Windows remains unsupported.

This follows the address-space enforcement path in Apple's published
[XNU resource implementation](https://github.com/apple-oss-distributions/xnu/blob/xnu-10002.1.13/bsd/kern/kern_resource.c#L1217).
The current [VM implementation](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/vm/vm_map.c)
also rejects limits below an existing map's size. A process with excessive startup
address-space use may therefore fail closed. Successful cross-compilation does
not establish successful startup or resource enforcement on macOS hardware.

A child-process test installs the limits and verifies that a 3 GiB reservation
fails. Its parent requires a completion marker, so accidentally selecting zero
tests cannot produce success. This ran on Linux. The existing deadline and output
limit controls also pass. Full Linux CLI tests: 134 passed; focused worker tests:
3 passed; CLI Clippy and formatting pass. The macOS ARM target checks, including
test code, pass. macOS runtime testing has not run.

Windows needs a separate implementation. Microsoft's
[job limit documentation](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_limit_information)
provides process execution-time and committed-memory limits. Committed memory is
not identical to the existing address-space metric; neither a working-set hint
nor a polling watchdog should be described as the same hard limit. No Windows
support claim or implementation follows from this API review.

OCR model provisioning, bounded inference, region omission tracking, alternatives,
native overlap, and recognition-to-source proofs remain outstanding. Recognition
confidence cannot discharge strict source coverage. These worker changes add no
new complete PDF pair and do not replace the fixed-panel final evaluation.

`worker-limits-checks.json` binds the changed source files and logs stored under
`benchmark/realworld/cache/source-completion/worker-limits-v1/`. Regenerable debug
builds were subsequently cleaned at the user's request; retained evidence,
frozen comparison executables and inputs were preserved.
