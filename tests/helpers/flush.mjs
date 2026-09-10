// Lets queued promise callbacks and any already-scheduled immediates settle.
export const flushMicrotasks = () => new Promise((resolve) => setImmediate(resolve));

// Waits past short timers (setTimeout debounce) scheduled by the code under test.
export const flushTimers = (ms = 10) => new Promise((resolve) => setTimeout(resolve, ms));
