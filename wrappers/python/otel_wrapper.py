"""Handler wrapper that signals invocation completion to the OpenTelemetry
Lambda extension.

Loaded in place of the original handler by the ``otel-handler`` exec
wrapper script, which records the original handler string in
``ORIG_HANDLER``. After each invocation (success or failure) the wrapper
posts to the extension's ``/invocation/complete`` endpoint so the
extension can flush telemetry in the post-invocation window before the
execution environment freezes.

The completion signal is best-effort: it times out quickly and never
raises, so a missing or unhealthy extension cannot fail the function.
"""

import importlib
import os
import urllib.request

_RECEIVER_PORT = os.environ.get("LAMBDA_OTEL_RECEIVER_HTTP_PORT", "4318")
_COMPLETE_URL = f"http://127.0.0.1:{_RECEIVER_PORT}/invocation/complete"
_SIGNAL_TIMEOUT_SECONDS = float(os.environ.get("LAMBDA_OTEL_COMPLETION_TIMEOUT_MS", "50")) / 1000.0

_original_handler = None


def _load_original_handler():
    """Loads the original handler named by ``ORIG_HANDLER``.

    Accepts both dotted (``src.app.handler``) and slashed
    (``src/app.handler``) module paths, matching Lambda's own handler
    resolution.
    """
    original = os.environ.get("ORIG_HANDLER")
    if not original:
        raise RuntimeError(
            "ORIG_HANDLER is not set; the otel-handler wrapper script must run first"
        )

    module_path, _, function_name = original.rpartition(".")
    if not module_path:
        raise RuntimeError(f'Invalid ORIG_HANDLER "{original}"; expected "module.function"')

    module = importlib.import_module(module_path.replace("/", "."))
    handler = getattr(module, function_name)
    if not callable(handler):
        raise RuntimeError(f'Handler "{original}" is not callable')
    return handler


def _signal_complete(request_id):
    """Signals invocation completion to the extension.

    Best-effort: bounded by a short timeout and swallows every error so
    signalling problems can never fail the invocation.
    """
    request = urllib.request.Request(_COMPLETE_URL, data=b"", method="POST")
    if request_id:
        request.add_header("Lambda-Request-Id", request_id)
    try:
        urllib.request.urlopen(request, timeout=_SIGNAL_TIMEOUT_SECONDS).close()
    except Exception:
        pass


def handler(event, context):
    """The handler Lambda invokes in place of the original."""
    global _original_handler
    if _original_handler is None:
        _original_handler = _load_original_handler()
    try:
        return _original_handler(event, context)
    finally:
        _signal_complete(getattr(context, "aws_request_id", None))
