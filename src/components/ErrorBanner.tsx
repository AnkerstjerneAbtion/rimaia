import type { RimaiaError } from "../types";

interface ErrorBannerProps {
  error: RimaiaError;
  onDismiss?: () => void;
}

export function ErrorBanner({ error, onDismiss }: ErrorBannerProps) {
  return (
    <div className="error-banner" role="alert">
      <span className="error-code">{error.code.replace("_", " ")}</span>
      <span className="error-message">{error.message}</span>
      {onDismiss && (
        <button type="button" className="error-dismiss" onClick={onDismiss}>
          Dismiss
        </button>
      )}
    </div>
  );
}
