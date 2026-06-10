import { useNavigate } from '@tanstack/react-router';

// Full-window preview of `/calm/_design/Report.html`. Reached from the
// TitleBar's "Report design" link; the app shell (TitleBar + Sidebar) is
// suppressed by CalmApp when the active route is `/_design`, so the iframe
// genuinely owns the viewport. A small fixed "Back to app" pill in the
// top-left navigates home — refreshing or hitting browser-back both also
// dismiss the preview, since the URL itself is the toggle state.
//
// Uses raw useNavigate (not the typed Route shim in `app/navigation`) so
// `Route` doesn't gain a demo variant — keeps the production type surface
// clean while the design sandbox is still landing.

export function DesignPreviewPage() {
  const navigate = useNavigate();
  return (
    <>
      <iframe
        src="/calm/_design/Report.html"
        title="Report design preview"
        style={{
          position: 'fixed',
          inset: 0,
          width: '100%',
          height: '100%',
          border: 'none',
          background: '#fff',
        }}
      />
      <button
        type="button"
        onClick={() => void navigate({ to: '/' })}
        style={{
          position: 'fixed',
          top: 16,
          left: 16,
          zIndex: 10,
          display: 'inline-flex',
          alignItems: 'center',
          gap: 6,
          height: 32,
          padding: '0 14px',
          borderRadius: 99,
          border: '1px solid rgba(0,0,0,.12)',
          background: 'rgba(255,255,255,.85)',
          backdropFilter: 'blur(10px)',
          fontFamily: 'inherit',
          fontSize: 12.5,
          fontWeight: 500,
          color: '#222',
          cursor: 'pointer',
          boxShadow: '0 1px 2px rgba(0,0,0,.06), 0 8px 24px -8px rgba(0,0,0,.15)',
        }}
      >
        ← Back to app
      </button>
    </>
  );
}
