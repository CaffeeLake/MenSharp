// MenSharp: the Udon whitelist of the SDK this project has installed.
//
// The compiler embeds a whitelist (every extern signature Udon exposes)
// dumped from a live SDK. A beta SDK, or a closed-beta one handed out for
// work, exposes externs that list has never heard of — and compiling
// against it would turn every one of them into "not exposed by Udon". So
// the editor dumps the whitelist of the SDK it actually has loaded
// (UdonEditorManager knows it; the dlls alone do not say what the SDK
// registers) and hands it to the compiler with `--udon-nodes`.
//
// Nobody has to ask for it. The dump is keyed by a fingerprint of the SDK
// (its package versions, and the module ids of its assemblies — a closed
// beta may well reuse a version string, a rebuilt dll never keeps its id),
// so it is made once per SDK and reused until the SDK changes. The dump's
// path is part of the compile signature, so a changed SDK also recompiles
// every program: going back from a beta to the live SDK turns a call to a
// beta-only extern into a compile error instead of a halt at run time.
//
// The dump stays in Library/, which never travels with the project: data
// derived from an SDK under NDA must not end up in someone's repository.

#if UNITY_EDITOR
using System;
using System.Collections.Generic;
using System.IO;
using System.Reflection;
using System.Text;
using System.Text.RegularExpressions;
using UnityEditor;
using UnityEngine;
using VRC.Udon.Editor;
using VRC.Udon.Graph;
using PackageInfo = UnityEditor.PackageManager.PackageInfo;

public static class MenSharpUdonNodes
{
    private const string DumpFolder = "Library/MenSharp/udon-nodes";

    /// The fingerprint of the SDK in this domain. Assemblies do not change
    /// without a domain reload, which resets it.
    private static string fingerprint;

    /// A failed dump is reported once per domain, not once per save.
    private static bool failureReported;

    /// The dump for the installed SDK, made now if there is none yet; `null`
    /// when it cannot be made (the compiler then uses its embedded list, and
    /// the console has said why).
    public static string EnsureDump()
    {
        string path;
        try
        {
            path = $"{DumpFolder}/{Fingerprint()}.json";
            if (File.Exists(path))
            {
                return path;
            }
        }
        catch (Exception exception)
        {
            ReportFailure(exception);
            return null;
        }
        try
        {
            EditorUtility.DisplayProgressBar(
                "MenSharp", "Reading the Udon whitelist of the installed VRChat SDK…", 0.5f);
            Write(path, SdkLabel(), true);
            return path;
        }
        catch (Exception exception)
        {
            ReportFailure(exception);
            return null;
        }
        finally
        {
            EditorUtility.ClearProgressBar();
        }
    }

    /// Throws the dump away and compiles again, which makes a fresh one —
    /// should a dump ever look wrong.
    [MenuItem("MenSharp/Regenerate Udon Whitelist")]
    public static void Regenerate()
    {
        try
        {
            if (Directory.Exists(DumpFolder))
            {
                Directory.Delete(DumpFolder, true);
            }
        }
        catch (Exception exception)
        {
            Debug.LogError($"MenSharp: could not remove {DumpFolder}: {exception.Message}");
            return;
        }
        fingerprint = null;
        failureReported = false;
        MenSharpCompiler.CompileAll();
    }

    /// The same dump, to a file of one's choosing: what `data/udon/` in the
    /// MenSharp repository is made from (and `--udon-nodes` reads, for a
    /// compile outside the editor).
    [MenuItem("MenSharp/Export Udon Whitelist…")]
    public static void Export()
    {
        string path = EditorUtility.SaveFilePanel(
            "Export the Udon whitelist", "", Application.unityVersion + ".json", "json");
        if (string.IsNullOrEmpty(path))
        {
            return;
        }
        int count = Write(path, SdkLabel(), false);
        Debug.Log($"MenSharp: wrote {count} Udon node definitions to {path}");
    }

    /// Writes every node definition the SDK has, in the schema
    /// `src/men-sharp-codegen/src/externs.rs` reads. With `cached`, the file
    /// is the dump the compiler is given: it replaces the dumps of earlier
    /// SDKs, and what changed since the last of them goes to the console.
    private static int Write(string path, string sdk, bool cached)
    {
        var text = new StringBuilder(1 << 24);
        var externs = new HashSet<string>(StringComparer.Ordinal);
        text.Append("{\n");
        text.Append("  \"unityVersion\": ").Append(Quote(Application.unityVersion)).Append(",\n");
        text.Append("  \"sdk\": ").Append(Quote(sdk)).Append(",\n");
        text.Append("  \"nodes\": [\n");
        bool firstNode = true;
        int count = 0;
        foreach (UdonNodeDefinition definition in UdonEditorManager.Instance.GetNodeDefinitions())
        {
            if (definition == null || definition.fullName == null)
            {
                continue;
            }
            if (!firstNode)
            {
                text.Append(",\n");
            }
            firstNode = false;
            count++;
            if (definition.fullName.Contains(".__"))
            {
                externs.Add(definition.fullName);
            }
            text.Append("    {\"fullName\": ").Append(Quote(definition.fullName));
            text.Append(", \"relatedType\": ").Append(Quote(TypeName(definition.type)));
            text.Append(", \"parameters\": [");
            bool firstParameter = true;
            foreach (UdonNodeParameter parameter in definition.parameters)
            {
                if (!firstParameter)
                {
                    text.Append(", ");
                }
                firstParameter = false;
                text.Append("{\"name\": ").Append(Quote(parameter.name));
                text.Append(", \"type\": ").Append(Quote(TypeName(parameter.type)));
                text.Append(", \"kind\": ").Append(Quote(parameter.parameterType.ToString()));
                text.Append('}');
            }
            text.Append("]}");
        }
        text.Append("\n  ]\n}\n");
        if (externs.Count == 0)
        {
            // an SDK not done initialising, or one whose internals moved:
            // an empty list would make every extern an error in user code
            throw new InvalidOperationException("the SDK reported no extern nodes");
        }

        string directory = Path.GetDirectoryName(path);
        if (!string.IsNullOrEmpty(directory))
        {
            Directory.CreateDirectory(directory);
        }
        // written beside and moved into place: a compile never reads half
        // a dump, and a dump cut short by a crash is never taken for one
        string partial = path + ".partial";
        File.WriteAllText(partial, text.ToString(), new UTF8Encoding(false));
        if (File.Exists(path))
        {
            File.Delete(path);
        }
        File.Move(partial, path);

        if (cached)
        {
            ReplaceEarlierDumps(path, sdk, externs);
        }
        return count;
    }

    /// Deletes the dumps of earlier SDKs, after telling the console what
    /// the newest of them did not have (and had, that this SDK has not).
    private static void ReplaceEarlierDumps(string path, string sdk, HashSet<string> externs)
    {
        string previous = null;
        DateTime newest = DateTime.MinValue;
        foreach (string file in Directory.GetFiles(DumpFolder))
        {
            if (Path.GetFullPath(file) == Path.GetFullPath(path))
            {
                continue;
            }
            DateTime written = File.GetLastWriteTimeUtc(file);
            if (file.EndsWith(".json", StringComparison.Ordinal) && written > newest)
            {
                newest = written;
                previous = file;
            }
        }

        var summary = new StringBuilder();
        summary.Append($"MenSharp: compiling against the Udon whitelist of the installed SDK ({sdk}): ")
            .Append($"{externs.Count} externs");
        if (previous != null)
        {
            var before = ExternsIn(File.ReadAllText(previous));
            var added = new List<string>();
            var removed = new List<string>();
            foreach (string signature in externs)
            {
                if (!before.Contains(signature))
                {
                    added.Add(signature);
                }
            }
            foreach (string signature in before)
            {
                if (!externs.Contains(signature))
                {
                    removed.Add(signature);
                }
            }
            summary.Append($", {added.Count} new and {removed.Count} gone since the previous SDK.");
            AppendList(summary, "new", added);
            AppendList(summary, "gone", removed);
        }
        else
        {
            summary.Append('.');
        }
        Debug.Log(summary.ToString());

        foreach (string file in Directory.GetFiles(DumpFolder))
        {
            if (Path.GetFullPath(file) != Path.GetFullPath(path))
            {
                File.Delete(file);
            }
        }
    }

    private static void AppendList(StringBuilder summary, string heading, List<string> signatures)
    {
        const int shown = 200;
        if (signatures.Count == 0)
        {
            return;
        }
        signatures.Sort(StringComparer.Ordinal);
        summary.Append("\n\n").Append(heading).Append(':');
        for (int index = 0; index < signatures.Count && index < shown; index++)
        {
            summary.Append("\n  ").Append(signatures[index]);
        }
        if (signatures.Count > shown)
        {
            summary.Append($"\n  … and {signatures.Count - shown} more");
        }
    }

    private static readonly Regex FullName = new Regex("\"fullName\": \"([^\"\\\\]*)\"");

    private static HashSet<string> ExternsIn(string dump)
    {
        var externs = new HashSet<string>(StringComparer.Ordinal);
        foreach (Match match in FullName.Matches(dump))
        {
            string name = match.Groups[1].Value;
            if (name.Contains(".__"))
            {
                externs.Add(name);
            }
        }
        return externs;
    }

    /// The installed SDK, hashed: the Unity version, every VRChat package
    /// with its version, and the module id of every VRChat assembly loaded.
    public static string Fingerprint()
    {
        if (fingerprint != null)
        {
            return fingerprint;
        }
        var lines = new List<string> { "unity " + Application.unityVersion };
        foreach (PackageInfo package in PackageInfo.GetAllRegisteredPackages())
        {
            if (package.name.StartsWith("com.vrchat.", StringComparison.Ordinal))
            {
                lines.Add($"package {package.name} {package.version}");
            }
        }
        foreach (Assembly assembly in AppDomain.CurrentDomain.GetAssemblies())
        {
            if (assembly.IsDynamic || !IsSdkAssembly(assembly))
            {
                continue;
            }
            lines.Add($"assembly {assembly.GetName().Name} {assembly.ManifestModule.ModuleVersionId}");
        }
        lines.Sort(StringComparer.Ordinal);
        using (var sha = System.Security.Cryptography.SHA1.Create())
        {
            byte[] hash = sha.ComputeHash(Encoding.UTF8.GetBytes(string.Join("\n", lines)));
            fingerprint = BitConverter.ToString(hash).Replace("-", "").Substring(0, 16).ToLowerInvariant();
        }
        return fingerprint;
    }

    /// An assembly the Udon whitelist may come from: the SDK's own (VRC.*,
    /// VRCSDK3, …, compiled from its sources into ScriptAssemblies) and any
    /// precompiled dll a VRChat package ships.
    private static bool IsSdkAssembly(Assembly assembly)
    {
        if (assembly.GetName().Name.StartsWith("VRC", StringComparison.Ordinal))
        {
            return true;
        }
        string location;
        try
        {
            location = assembly.Location;
        }
        catch (Exception)
        {
            return false;
        }
        return !string.IsNullOrEmpty(location)
            && location.Replace('\\', '/').Contains("/com.vrchat.");
    }

    /// How a dump names its SDK: the worlds package, which is the one that
    /// brings Udon (`com.vrchat.worlds 3.10.5`).
    private static string SdkLabel()
    {
        PackageInfo package = PackageInfo.FindForAssembly(typeof(UdonEditorManager).Assembly);
        return package != null ? $"{package.name} {package.version}" : "VRChat SDK";
    }

    private static void ReportFailure(Exception exception)
    {
        if (failureReported)
        {
            return;
        }
        failureReported = true;
        Debug.LogWarning(
            "MenSharp: could not read the Udon whitelist of the installed SDK, so programs are "
            + "compiled against the list built into MenSharp — externs only this SDK has will "
            + $"be reported as not exposed.\n{exception}");
    }

    private static string TypeName(Type type)
    {
        if (type == null)
        {
            return null;
        }
        return type.FullName ?? type.Name;
    }

    private static string Quote(string value)
    {
        if (value == null)
        {
            return "null";
        }
        var text = new StringBuilder(value.Length + 2);
        text.Append('"');
        foreach (char c in value)
        {
            switch (c)
            {
                case '"': text.Append("\\\""); break;
                case '\\': text.Append("\\\\"); break;
                case '\n': text.Append("\\n"); break;
                case '\r': text.Append("\\r"); break;
                case '\t': text.Append("\\t"); break;
                default:
                    if (c < 0x20)
                    {
                        text.Append("\\u").Append(((int)c).ToString("x4"));
                    }
                    else
                    {
                        text.Append(c);
                    }
                    break;
            }
        }
        text.Append('"');
        return text.ToString();
    }
}
#endif
