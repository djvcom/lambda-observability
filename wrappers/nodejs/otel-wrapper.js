/**
 * Handler wrapper that signals invocation completion to the OpenTelemetry
 * Lambda extension.
 *
 * Loaded in place of the original handler by the `otel-handler` exec
 * wrapper script, which records the original handler string in
 * `ORIG_HANDLER`. After each invocation (success or failure) the wrapper
 * posts to the extension's `/invocation/complete` endpoint so the
 * extension can flush telemetry in the post-invocation window before the
 * execution environment freezes.
 *
 * The completion signal is best-effort: it times out quickly and never
 * throws, so a missing or unhealthy extension cannot fail the function.
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { pathToFileURL } = require('node:url');

const RECEIVER_PORT = process.env.LAMBDA_OTEL_RECEIVER_HTTP_PORT || '4318';
const COMPLETE_URL = `http://127.0.0.1:${RECEIVER_PORT}/invocation/complete`;
const SIGNAL_TIMEOUT_MS = 50;
const HANDLER_EXTENSIONS = ['.js', '.mjs', '.cjs'];

let cachedHandlerPromise;

/**
 * Loads the original handler named by `ORIG_HANDLER` from the function's
 * task root, supporting CommonJS and ES module handlers.
 *
 * @returns {Promise<Function>} the original handler function
 */
async function loadOriginalHandler() {
  const original = process.env.ORIG_HANDLER;
  if (!original) {
    throw new Error('ORIG_HANDLER is not set; the otel-handler wrapper script must run first');
  }

  const lastDot = original.lastIndexOf('.');
  if (lastDot <= 0) {
    throw new Error(`Invalid ORIG_HANDLER "${original}"; expected "file.export"`);
  }
  const modulePath = original.slice(0, lastDot);
  const exportName = original.slice(lastDot + 1);
  const taskRoot = process.env.LAMBDA_TASK_ROOT || process.cwd();

  for (const extension of HANDLER_EXTENSIONS) {
    const file = path.resolve(taskRoot, modulePath + extension);
    if (!fs.existsSync(file)) {
      continue;
    }
    const module = await import(pathToFileURL(file).href);
    const handler = module[exportName] ?? module.default?.[exportName];
    if (typeof handler !== 'function') {
      throw new Error(`Handler "${exportName}" in "${file}" is not a function`);
    }
    return handler;
  }

  throw new Error(`Cannot find handler module "${modulePath}" under "${taskRoot}"`);
}

/**
 * Invokes the handler, supporting both promise-returning and
 * callback-style signatures.
 *
 * @param {Function} handler the original handler
 * @param {*} event the invocation event
 * @param {*} context the invocation context
 * @returns {Promise<*>} the handler's result
 */
function invokeHandler(handler, event, context) {
  if (handler.length <= 2) {
    return Promise.resolve(handler(event, context));
  }
  return new Promise((resolve, reject) => {
    const result = handler(event, context, (error, value) =>
      error ? reject(error) : resolve(value),
    );
    if (result && typeof result.then === 'function') {
      result.then(resolve, reject);
    }
  });
}

/**
 * Signals invocation completion to the extension. Best-effort: bounded by
 * a short timeout and swallows every error so signalling problems can
 * never fail the invocation.
 *
 * @param {string|undefined} requestId the AWS request ID, when known
 */
async function signalComplete(requestId) {
  const headers = requestId ? { 'Lambda-Request-Id': requestId } : {};
  try {
    await fetch(COMPLETE_URL, {
      method: 'POST',
      headers,
      signal: AbortSignal.timeout(SIGNAL_TIMEOUT_MS),
    });
  } catch {}
}

/**
 * The handler Lambda invokes in place of the original.
 *
 * @param {*} event the invocation event
 * @param {*} context the invocation context
 * @returns {Promise<*>} the original handler's result
 */
exports.handler = async function handler(event, context) {
  cachedHandlerPromise ??= loadOriginalHandler();
  const originalHandler = await cachedHandlerPromise;
  try {
    return await invokeHandler(originalHandler, event, context);
  } finally {
    await signalComplete(context?.awsRequestId);
  }
};
