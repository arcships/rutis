export type ErrorCode = 'InvalidParams' | 'InterfaceMismatch' | 'UnsupportedCapability' | 'CapabilityDenied' | 'StaleObject' | 'ScopeClosed' | 'Cancelled' | 'DeadlineExceeded' | 'Unavailable' | 'Business'
export class ProtocolError extends Error {
  constructor(readonly code: ErrorCode, readonly stage: string, message: string,
    readonly execution: 'not_started' | 'unknown' = 'not_started') { super(message) }
}
