import { Eye, EyeOff, ExternalLink, KeyRound, LockKeyhole, RefreshCw } from "lucide-react";
import { useState } from "react";
import { Avatar } from "../App";
import { Banner, PageHeader } from "../components/common";
import { ConfirmDialog, useToast } from "../components/ui/overlay";
import { Badge, Button, Card, Field, Input, Spinner } from "../components/ui/primitives";
import { api, errorInfo } from "../lib/api";
import { errorTitle, sentence } from "../lib/errors";
import { HF_TOKENS_URL, openUrl } from "../lib/native";
import { useApp } from "../lib/state";
import type { ErrorInfo } from "../lib/types";
import { useGuard } from "../hooks/useGuard";

export function AccountPage() {
  const { account, refreshAccount, setAccount } = useApp();
  const toast = useToast();
  const [replacing, setReplacing] = useState(false);
  const [confirmOut, setConfirmOut] = useState(false);
  const [signingOut, setSigningOut] = useState(false);
  const [checking, setChecking] = useState(false);

  const signOut = async () => {
    setSigningOut(true);
    try {
      await api.signOut();
      setAccount({ state: "signed_out", environment_variable: account?.environment_variable });
      setConfirmOut(false);
      toast({ tone: "success", title: "Signed out", body: "The token was removed from Windows Credential Manager." });
    } catch (e) {
      toast({ tone: "error", title: "Could not sign out", body: errorInfo(e).message });
    } finally {
      setSigningOut(false);
    }
  };

  const recheck = async () => {
    setChecking(true);
    await refreshAccount(true);
    setChecking(false);
  };

  const state = account?.state;
  const showForm = state === "signed_out" || state === "invalid_token" || replacing;

  return (
    <>
      <PageHeader
        title="Account"
        description="Public models and datasets work without an account. Sign in to mount private or gated repositories and buckets."
      />

      <div className="flex flex-col gap-4">
        {!account && (
          <div className="flex items-center gap-2 py-6 text-[13px] text-fg-3" role="status">
            <Spinner /> Checking your account…
          </div>
        )}

        {account?.environment_variable && (
          <Banner title={`${account.environment_variable} is set on this PC`}>
            The command line uses that token before the one stored here. The desktop app always uses the token stored in
            Windows Credential Manager.
          </Banner>
        )}

        {state === "invalid_token" && (
          <Banner tone="warn" title="Your token no longer works">
            The token stored{account?.stored_account ? ` for ${account.stored_account}` : ""} was rejected by Hugging Face. It may
            have expired or been deleted. Sign in again with a new token.
          </Banner>
        )}

        {state === "unverified" && (
          <Banner
            tone="warn"
            title="Couldn't check your token"
            actions={
              <Button size="sm" icon={<RefreshCw className="size-3.5" />} onClick={() => void recheck()} loading={checking}>
                Check again
              </Button>
            }
          >
            {account?.error ? sentence(account.error.message) : "Hugging Face could not be reached."} A token is stored and will
            be used for new drives.
          </Banner>
        )}

        {(state === "signed_in" || state === "unverified" || state === "stored") && !replacing && (
          <Card className="p-5">
            <div className="flex flex-wrap items-center gap-4">
              <Avatar name={account?.account?.name ?? account?.stored_account ?? null} size={48} />
              <div className="min-w-0 flex-1">
                <p className="truncate text-[16px] font-semibold tracking-[-0.01em] text-fg">
                  {account?.account?.fullname || account?.account?.name || account?.stored_account || "Stored token"}
                </p>
                <p className="truncate text-[13px] text-fg-2">
                  {account?.account?.name ? `@${account.account.name}` : "Not verified yet"}
                </p>
              </div>
              <div className="flex gap-2">
                <Button size="sm" onClick={() => setReplacing(true)}>
                  Change token
                </Button>
                <Button size="sm" variant="danger" onClick={() => setConfirmOut(true)}>
                  Sign out
                </Button>
              </div>
            </div>
            {account?.account && (
              <dl className="mt-5 grid grid-cols-[140px_1fr] gap-x-4 gap-y-2.5 border-t border-border pt-4 text-[13px]">
                <dt className="text-fg-3">Token</dt>
                <dd className="flex items-center gap-2 text-fg">
                  {account.account.token_name ?? "Unnamed token"}
                  {account.account.token_role && <Badge tone="outline">{account.account.token_role}</Badge>}
                </dd>
                <dt className="text-fg-3">Organizations</dt>
                <dd className="text-fg">{account.account.orgs.length ? account.account.orgs.join(", ") : "None"}</dd>
                <dt className="text-fg-3">Stored in</dt>
                <dd className="text-fg">Windows Credential Manager</dd>
              </dl>
            )}
          </Card>
        )}

        {account && showForm && (
          <SignInForm
            onCancel={replacing ? () => setReplacing(false) : undefined}
            onSignedIn={(a) => {
              setAccount(a);
              setReplacing(false);
              toast({ tone: "success", title: `Signed in as ${a.account?.name ?? "your account"}` });
            }}
          />
        )}

        <Card className="flex gap-3 p-5 text-[13px] leading-relaxed text-fg-2">
          <LockKeyhole className="mt-0.5 size-4 shrink-0 text-fg-3" aria-hidden />
          <div>
            <p className="font-medium text-fg">How your token is kept</p>
            <p className="mt-1">
              The token is stored in Windows Credential Manager under your Windows account, never in a settings file. It is
              passed to the mount service over a private connection that only your account can open, and it never appears in
              logs, diagnostics or the command line of background processes.
            </p>
          </div>
        </Card>
      </div>

      <ConfirmDialog
        open={confirmOut}
        onOpenChange={setConfirmOut}
        title="Sign out?"
        body="The token is removed from Windows Credential Manager. Drives that are already mounted keep working until they are unmounted; private repositories and buckets cannot be mounted again until you sign in."
        confirmLabel="Sign out"
        danger
        busy={signingOut}
        onConfirm={() => void signOut()}
      />
    </>
  );
}

function SignInForm({ onSignedIn, onCancel }: { onSignedIn: (a: import("../lib/types").AccountStatus) => void; onCancel?: () => void }) {
  const [token, setToken] = useState("");
  const [visible, setVisible] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ErrorInfo | null>(null);
  const guard = useGuard();

  const looksWrong = token.trim().length > 0 && !/^hf_[A-Za-z0-9]{20,}$/.test(token.trim());

  const submit = async () => {
    if (!token.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      const status = await api.signIn(token.trim());
      setToken("");
      onSignedIn(status);
    } catch (e) {
      setError(errorInfo(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card className="p-5">
      <div className="flex items-start gap-3">
        <div className="grid size-9 shrink-0 place-items-center rounded-[11px] border border-border bg-surface-2">
          <KeyRound className="size-4 text-fg-2" aria-hidden />
        </div>
        <div>
          <h2 className="text-[15px] font-semibold text-fg">Sign in with an access token</h2>
          <p className="mt-0.5 text-[13px] text-fg-2">
            Create a token on Hugging Face (Read for repositories, Write to change buckets), then paste it here.
          </p>
        </div>
      </div>
      <form
        className="mt-5 flex flex-col gap-4"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <Field
          label="Access token"
          htmlFor="token"
          error={error ? `${errorTitle(error)}: ${sentence(error.message)}` : null}
          hint={looksWrong ? "Hugging Face tokens start with hf_." : "The token is checked with Hugging Face before it is saved."}
        >
          <div className="relative">
            <Input
              id="token"
              mono
              type={visible ? "text" : "password"}
              value={token}
              onChange={(e) => {
                setToken(e.target.value);
                setError(null);
              }}
              placeholder="hf_…"
              invalid={!!error}
              className="pr-11"
              autoFocus={!!onCancel}
              disabled={busy}
            />
            <button
              type="button"
              onClick={() => setVisible((v) => !v)}
              aria-label={visible ? "Hide token" : "Show token"}
              aria-pressed={visible}
              className="absolute right-1.5 top-1/2 grid size-7 -translate-y-1/2 place-items-center rounded-[8px] text-fg-3 hover:bg-surface-2 hover:text-fg"
            >
              {visible ? <EyeOff className="size-4" /> : <Eye className="size-4" />}
            </button>
          </div>
        </Field>
        <div className="flex flex-wrap items-center gap-2">
          <Button type="submit" variant="primary" loading={busy} disabled={!token.trim()}>
            {busy ? "Checking…" : "Sign in"}
          </Button>
          {onCancel && (
            <Button variant="ghost" onClick={onCancel} disabled={busy}>
              Cancel
            </Button>
          )}
          <Button variant="ghost" className="ml-auto" icon={<ExternalLink className="size-3.5" />} onClick={() => guard(openUrl(HF_TOKENS_URL), "Could not open the browser")}>
            Create a token
          </Button>
        </div>
      </form>
    </Card>
  );
}
