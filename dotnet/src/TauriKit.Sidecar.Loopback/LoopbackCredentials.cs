using System.Buffers.Text;
using System.Collections.Concurrent;
using System.Security.Cryptography;
using System.Text;

namespace TauriKit.Sidecar.Loopback;

/// <summary>
/// Who presented a credential: a kind the app names — the owner-facing shell, an agent session, a plugin — and,
/// optionally, which one of that kind (a session id, say).
/// </summary>
/// <param name="Kind">What the caller is. Opaque to this library; route policies name the kinds they admit.</param>
/// <param name="Subject">Which caller of that kind, or <c>null</c> when there is only one.</param>
public sealed record LoopbackCaller(string Kind, string? Subject = null);

/// <summary>
/// The credentials a loopback backend accepts, each tied to the caller it was given to. A backend that runs
/// less-trusted workers beside an owner-facing shell gives each worker its own credential, so a worker holds no more
/// than its own kind admits and can be told apart from the others — and revoked when it is done.
/// Held in memory only: nothing here touches the disk (a discovery file carries one credential, see
/// <see cref="DiscoveryFile"/>).
/// </summary>
/// <remarks>
/// A presented credential is looked up by its SHA-256 digest, so the time a lookup takes depends on the digest, which a
/// caller cannot steer toward a registered credential — not on how much of a guess was right.
/// </remarks>
public sealed class LoopbackCredentials
{
    /// <summary>The shortest credential accepted — a short one is a mistake, not a secret.</summary>
    public const int MinimumLength = 32;

    private readonly ConcurrentDictionary<string, LoopbackCaller> _callers = new(StringComparer.Ordinal);

    /// <summary>Makes a new credential (32 random bytes, base64url) for <paramref name="kind"/> and registers it.</summary>
    public string Issue(string kind, string? subject = null)
    {
        var credential = Base64Url.EncodeToString(RandomNumberGenerator.GetBytes(32));
        Register(credential, kind, subject);
        return credential;
    }

    /// <summary>
    /// Registers a credential made elsewhere — the one a parent process passed in an environment variable
    /// (<see cref="LoopbackHost.ReadToken"/>), say — for <paramref name="kind"/>.
    /// </summary>
    /// <exception cref="ArgumentException">The credential is shorter than <see cref="MinimumLength"/>, or the kind is empty.</exception>
    /// <exception cref="InvalidOperationException">The credential is registered already.</exception>
    public void Register(string credential, string kind, string? subject = null)
    {
        ArgumentNullException.ThrowIfNull(credential);
        ArgumentException.ThrowIfNullOrEmpty(kind);
        if (credential.Length < MinimumLength)
            throw new ArgumentException($"A credential has at least {MinimumLength} characters.", nameof(credential));
        if (!_callers.TryAdd(Key(credential), new LoopbackCaller(kind, subject)))
            throw new InvalidOperationException("The credential is registered already.");
    }

    /// <summary>Stops accepting <paramref name="credential"/> (its worker exited, its session ended). <c>false</c> when it was not registered.</summary>
    public bool Revoke(string credential)
    {
        ArgumentNullException.ThrowIfNull(credential);
        return _callers.TryRemove(Key(credential), out _);
    }

    /// <summary>The caller <paramref name="credential"/> was given to, or <c>null</c> when it is not one of these.</summary>
    public LoopbackCaller? Resolve(string? credential) =>
        credential is { Length: >= MinimumLength } && _callers.TryGetValue(Key(credential), out var caller) ? caller : null;

    private static string Key(string credential) => Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(credential)));
}
