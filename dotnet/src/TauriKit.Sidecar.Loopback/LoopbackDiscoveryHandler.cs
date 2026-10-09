using System.Net;
using System.Net.Http.Headers;

namespace TauriKit.Sidecar.Loopback;

/// <summary>
/// The caller's side of a backend found by its discovery file (<see cref="DiscoveryFile"/>): every request goes to the
/// port the file names, on 127.0.0.1, with <c>Authorization: Bearer</c> set to the credential it holds. Every start of
/// the backend issues a new credential and usually gets a new port, so when the backend answers 401 or cannot be
/// reached, the file is read again and — when it now names another run — the request is sent once more. A request
/// sent twice must have content that can be read twice (a string, bytes, a form — not a one-pass stream).
/// </summary>
/// <example>
/// <code>
/// using var client = LoopbackDiscoveryHandler.CreateClient(discoveryPath);
/// using var answer = await client.PostAsync("/shutdown", content: null);
/// </code>
/// </example>
public sealed class LoopbackDiscoveryHandler : DelegatingHandler
{
    private readonly string _discoveryPath;
    private LoopbackDiscovery? _current;

    /// <summary>A handler for the backend <paramref name="discoveryPath"/> names, sending through a handler that bypasses any proxy.</summary>
    public LoopbackDiscoveryHandler(string discoveryPath)
        : this(discoveryPath, new SocketsHttpHandler { UseProxy = false })
    {
    }

    /// <summary>A handler for the backend <paramref name="discoveryPath"/> names, sending through <paramref name="innerHandler"/>.</summary>
    public LoopbackDiscoveryHandler(string discoveryPath, HttpMessageHandler innerHandler)
        : base(innerHandler)
    {
        ArgumentException.ThrowIfNullOrEmpty(discoveryPath);
        _discoveryPath = discoveryPath;
    }

    /// <summary>
    /// An <see cref="HttpClient"/> for the backend <paramref name="discoveryPath"/> names: relative addresses
    /// (<c>"/health"</c>) go to it, whatever port it runs on now.
    /// </summary>
    public static HttpClient CreateClient(string discoveryPath) =>
        new(new LoopbackDiscoveryHandler(discoveryPath)) { BaseAddress = new Uri("http://127.0.0.1/") };

    /// <inheritdoc />
    /// <exception cref="LoopbackUnavailableException">The file names no running backend.</exception>
    protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(request);
        var used = Volatile.Read(ref _current) ?? Read();
        HttpResponseMessage response;
        try
        {
            response = await SendToAsync(used, request, cancellationToken).ConfigureAwait(false);
        }
        catch (HttpRequestException unreachable) when (unreachable is not LoopbackUnavailableException)
        {
            // Gone, or restarted on another port: only a file naming another run is worth a second try.
            var again = ReadAgain(used, unreachable);
            if (again is null) throw;
            return await SendToAsync(again, request, cancellationToken).ConfigureAwait(false);
        }

        if (response.StatusCode != HttpStatusCode.Unauthorized) return response;
        LoopbackDiscovery? fresh;
        try
        {
            fresh = ReadAgain(used, inner: null);
        }
        catch (LoopbackUnavailableException)
        {
            return response; // the 401 is the backend's answer; a file gone since does not change it
        }
        if (fresh is null) return response;
        response.Dispose();
        return await SendToAsync(fresh, request, cancellationToken).ConfigureAwait(false);
    }

    private Task<HttpResponseMessage> SendToAsync(LoopbackDiscovery backend, HttpRequestMessage request, CancellationToken cancellationToken)
    {
        if (request.RequestUri is not { IsAbsoluteUri: true } address)
            throw new InvalidOperationException("The request has no absolute address; give the client a base address (CreateClient does).");
        request.RequestUri = new UriBuilder(address) { Scheme = Uri.UriSchemeHttp, Host = "127.0.0.1", Port = backend.Port }.Uri;
        request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", backend.Token);
        return base.SendAsync(request, cancellationToken);
    }

    private LoopbackDiscovery Read()
    {
        if (!DiscoveryFile.TryRead(_discoveryPath, out var found, out var problem))
            throw new LoopbackUnavailableException(problem, inner: null);
        Volatile.Write(ref _current, found);
        return found;
    }

    // The file's record when it names another run than the one just used; null when it still names that one.
    private LoopbackDiscovery? ReadAgain(LoopbackDiscovery used, Exception? inner)
    {
        if (!DiscoveryFile.TryRead(_discoveryPath, out var found, out var problem))
            throw new LoopbackUnavailableException(problem, inner);
        if (found == used) return null;
        Volatile.Write(ref _current, found);
        return found;
    }
}

/// <summary>The discovery file names no backend to call — none published one, or the one that did is gone.</summary>
public sealed class LoopbackUnavailableException : HttpRequestException
{
    /// <summary>Why the file named no backend.</summary>
    public DiscoveryProblem Problem { get; }

    /// <summary>A backend that cannot be found, for <paramref name="problem"/>.</summary>
    public LoopbackUnavailableException(DiscoveryProblem problem, Exception? inner)
        : base($"No loopback backend to call: the discovery file is {problem.ToString().ToLowerInvariant()}.", inner)
    {
        Problem = problem;
    }
}
