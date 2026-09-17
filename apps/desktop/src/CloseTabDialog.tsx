interface Props {
  docName: string;
  onSave: () => void;
  onDiscard: () => void;
  onCancel: () => void;
}

/** Standard three-way "unsaved changes" prompt (Save / Don't Save / Cancel) -
 * a single dialog instead of two chained yes/no confirms, so closing an
 * unsaved tab is one decision, not a forced march through extra prompts. */
export default function CloseTabDialog({ docName, onSave, onDiscard, onCancel }: Props) {
  return (
    <div className="settings-overlay" onClick={onCancel}>
      <div className="close-tab-dialog" onClick={(e) => e.stopPropagation()}>
        <p>
          Save changes to <strong>{docName}</strong> before closing? It hasn't been saved to disk yet.
        </p>
        <div className="close-tab-actions">
          <button onClick={onDiscard} className="discard-btn">
            Don't Save
          </button>
          <button onClick={onCancel}>Cancel</button>
          <button onClick={onSave} className="save-btn">
            Save...
          </button>
        </div>
      </div>
    </div>
  );
}
