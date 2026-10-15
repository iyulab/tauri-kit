using System.Security.Claims;
using System.Text.Encodings.Web;
using Microsoft.AspNetCore.Authentication;
using Microsoft.AspNetCore.Authorization;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Options;

namespace TauriKit.Sidecar.Loopback;

/// <summary>
/// Bearer authentication against <see cref="LoopbackCredentials"/>, with every route requiring it unless it is named
/// open: the fallback policy demands an authenticated caller, so a route added later is closed until someone marks it
/// <c>AllowAnonymous()</c>. A request without a registered credential is answered 401; one whose caller's kind a
/// route does not admit (<see cref="RequireCaller{TBuilder}"/>), 403.
/// </summary>
/// <example>
/// <code>
/// var credentials = new LoopbackCredentials();
/// var owner = credentials.Issue("owner");
/// var builder = LoopbackHost.CreateSlimBuilder(args);
/// builder.Services.AddLoopbackAuthentication(credentials);
/// var app = builder.Build();
/// app.MapGet("/health", () => "ok").AllowAnonymous();
/// app.MapPost("/settings", Save).RequireCaller("owner");
/// app.MapGet("/sessions/current", (HttpContext c) => c.User.LoopbackCaller()!.Subject);
/// </code>
/// </example>
public static class LoopbackAuthentication
{
    /// <summary>The authentication scheme's name.</summary>
    public const string Scheme = "LoopbackBearer";

    /// <summary>The claim carrying the caller's <see cref="LoopbackCaller.Kind"/>.</summary>
    public const string KindClaim = "loopback_kind";

    /// <summary>The claim carrying the caller's <see cref="LoopbackCaller.Subject"/>, when it has one.</summary>
    public const string SubjectClaim = "loopback_subject";

    /// <summary>
    /// Authenticates requests by the bearer credentials in <paramref name="credentials"/> and makes that the fallback
    /// policy: every route requires a registered caller unless it allows anonymous requests.
    /// </summary>
    public static IServiceCollection AddLoopbackAuthentication(this IServiceCollection services, LoopbackCredentials credentials)
    {
        ArgumentNullException.ThrowIfNull(services);
        ArgumentNullException.ThrowIfNull(credentials);
        services.AddSingleton(credentials);
        services.AddAuthentication(Scheme).AddScheme<AuthenticationSchemeOptions, LoopbackBearerHandler>(Scheme, null);
        services.AddAuthorizationBuilder()
            .SetFallbackPolicy(new AuthorizationPolicyBuilder(Scheme).RequireAuthenticatedUser().Build());
        return services;
    }

    /// <summary>Admits only callers of one of <paramref name="kinds"/> to the endpoints <paramref name="builder"/> builds.</summary>
    public static TBuilder RequireCaller<TBuilder>(this TBuilder builder, params string[] kinds) where TBuilder : IEndpointConventionBuilder
    {
        ArgumentNullException.ThrowIfNull(builder);
        ArgumentOutOfRangeException.ThrowIfZero(kinds.Length);
        return builder.RequireAuthorization(new AuthorizationPolicyBuilder(Scheme).RequireClaim(KindClaim, kinds).Build());
    }

    /// <summary>The caller a request authenticated as, or <c>null</c> on an open route reached without a credential.</summary>
    public static LoopbackCaller? LoopbackCaller(this ClaimsPrincipal user)
    {
        ArgumentNullException.ThrowIfNull(user);
        return user.FindFirst(KindClaim)?.Value is { } kind ? new LoopbackCaller(kind, user.FindFirst(SubjectClaim)?.Value) : null;
    }
}

/// <summary>Reads <c>Authorization: Bearer &lt;credential&gt;</c> and resolves it with <see cref="LoopbackCredentials"/>.</summary>
internal sealed class LoopbackBearerHandler(
    IOptionsMonitor<AuthenticationSchemeOptions> options, ILoggerFactory logger, UrlEncoder encoder, LoopbackCredentials credentials)
    : AuthenticationHandler<AuthenticationSchemeOptions>(options, logger, encoder)
{
    private const string Prefix = "Bearer ";

    protected override Task<AuthenticateResult> HandleAuthenticateAsync()
    {
        var header = Request.Headers.Authorization.ToString();
        if (!header.StartsWith(Prefix, StringComparison.Ordinal)) return Task.FromResult(AuthenticateResult.NoResult());
        if (credentials.Resolve(header[Prefix.Length..]) is not { } caller)
            return Task.FromResult(AuthenticateResult.Fail("The credential is not one this backend accepts."));
        List<Claim> claims = [new(LoopbackAuthentication.KindClaim, caller.Kind)];
        if (caller.Subject is { } subject) claims.Add(new(LoopbackAuthentication.SubjectClaim, subject));
        var principal = new ClaimsPrincipal(new ClaimsIdentity(claims, LoopbackAuthentication.Scheme));
        return Task.FromResult(AuthenticateResult.Success(new AuthenticationTicket(principal, LoopbackAuthentication.Scheme)));
    }

    /// <summary>
    /// Answers 401 with <c>WWW-Authenticate: Bearer</c> (RFC 6750) — with <c>error="invalid_token"</c> when a credential
    /// was sent and refused — so a caller can tell what is asked of it instead of reading an empty answer.
    /// </summary>
    protected override Task HandleChallengeAsync(AuthenticationProperties properties)
    {
        Response.StatusCode = StatusCodes.Status401Unauthorized;
        Response.Headers.WWWAuthenticate = Request.Headers.Authorization.ToString().StartsWith(Prefix, StringComparison.Ordinal)
            ? "Bearer error=\"invalid_token\""
            : "Bearer";
        return Task.CompletedTask;
    }
}
