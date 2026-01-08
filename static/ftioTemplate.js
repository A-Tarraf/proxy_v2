async function populateParameters() {
    fetch('/ftio/args', { method: 'GET' })
        .then(response => response.json())
        .then(data => {
            document.getElementById('samplingRateField').value = data.freq ?? 10.0;
            document.getElementById('memoryLimitField').value = data.memory_limit ?? '';
            document.getElementById('tsField').value = data.ts ?? '';
            document.getElementById('teField').value = data.te ?? '';
            document.getElementById('techniqueMenu').value = data.transformation ?? 'dft';
            document.getElementById('levelField').value = data.level ?? '';
            document.getElementById('waveletField').value = data.wavelet ?? '';
            document.getElementById('outlierMenu').value = data.outlier ?? 'z-score';
            document.getElementById('periodicityMenu').value = data.periodicity_detection ?? '';
            document.getElementById('toleranceField').value = data.tol ?? 0.8;
            document.getElementById('dtwBox').checked = data.dtw ?? false;
            document.getElementById('spectrumMenu').value = data.no_psd ? 'amplitude' : 'powerDensity';
            document.getElementById('nFreqField').value = data.n_freq ?? 10;
            document.getElementById('fourierFitBox').checked = data.fourier_fit ?? false;
            document.getElementById('autocorrelationBox').checked = data.autocorrelation ?? false;
            document.getElementById('windowAdaptationField').value = data.window_adaptation ?? '';
            document.getElementById('hitsField').value = data.hits ?? '';
            document.getElementById('filterTypeField').value = data.filter_type ?? '';
            document.getElementById('filterCutoffField').value = data.filter_cutoff ?? '';
            document.getElementById('filterCutoff2Field').value = data.filter_cutoff2 ?? '';
            document.getElementById('filterOrderField').value = data.filter_order ?? '';
            document.getElementById('cmdInput').value = data.custom_args ?? '';

            toggleMemoryLimit();
            toggleDecompLevel();
            toggleWaveletType();
            toggleHitsNeeded();
            toggleFilter();
        });
}

async function saveParameters() {
    const payload = convertParameters();

    fetch('/ftio/args', {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload)
    }).then(response => {
        if (response.ok) {
            console.log('Parameters saved successfully!');
        } else {
            console.error('Failed to save parameters.');
        }
    });
}

function convertParameters() {
    const customArgsRaw = document.getElementById('cmdInput').value || "";
    const customArgs = parseCustomArgs(customArgsRaw);

    // Use custom flag if present, else UI field
    const get = (flag, fallback) => customArgs[flag] !== undefined ? customArgs[flag] : fallback;

    const payload = {
        freq: parseFloat(get("freq", document.getElementById('samplingRateField').value)),
        memory_limit: parseFloat(get("memory_limit",
            parseFloat(document.getElementById('samplingRateField').value) === -1
                ? document.getElementById('memoryLimitField').value
                : null)),
        ts: parseFloat(get("ts", document.getElementById('tsField').value)) || null,
        te: parseFloat(get("te", document.getElementById('teField').value)) || null,
        transformation: get("transformation", document.getElementById('techniqueMenu').value),
        level: document.getElementById('techniqueMenu').value === "wave_disc"
            ? parseInt(get("level", document.getElementById('levelField').value))
            : null,
        wavelet: get("wavelet",
            (["wave_disc", "wave_cont"].includes(document.getElementById('techniqueMenu').value))
                ? document.getElementById('waveletField').value
                : null),
        outlier: get("outlier", document.getElementById('outlierMenu').value),
        periodicity_detection: get("periodicity_detection", document.getElementById('periodicityMenu').value || null),
        tol: parseFloat(get("tol", document.getElementById('toleranceField').value)),
        dtw: get("dtw", document.getElementById('dtwBox').checked),
        no_psd: get("no_psd", document.getElementById('spectrumMenu').value === 'amplitude'),
        n_freq: parseInt(get("n_freq", document.getElementById('nFreqField').value)),
        fourier_fit: get("fourier_fit", document.getElementById('fourierFitBox').checked),
        autocorrelation: get("autocorrelation", document.getElementById('autocorrelationBox').checked),
        window_adaptation: get("window_adaptation", document.getElementById('windowAdaptationField').value || null),
        hits: get("hits",
            document.getElementById('windowAdaptationField').value === 'frequency_hits'
                ? document.getElementById('hitsField').value
                : null),
        filter_type: get("filter_type", document.getElementById('filterTypeField').value || null),
        filter_cutoff: parseFloat(get("filter_cutoff",
            (document.getElementById('filterTypeField').value && document.getElementById('filterCutoffField').value)
                ? document.getElementById('filterCutoffField').value
                : null)),
        filter_cutoff2: parseFloat(get("filter_cutoff2",
            (document.getElementById('filterTypeField').value === "bandpass" && document.getElementById('filterCutoff2Field').value)
                ? document.getElementById('filterCutoff2Field').value
                : null)),
        filter_order: parseInt(get("filter_order",
            (document.getElementById('filterTypeField').value && document.getElementById('filterOrderField').value)
                ? document.getElementById('filterOrderField').value
                : null)),
        custom_args: customArgsRaw || null
    };
    return payload;
}

function copyParameters() {
    const nameMap = {
        samplingRateField: "freq",
        memoryLimitField: "memory_limit",
        tsField: "ts",
        teField: "te",
        techniqueMenu: "transformation",
        levelField: "level",
        waveletField: "wavelet",
        outlierMenu: "outlier",
        periodicityMenu: "periodicity_detection",
        toleranceField: "tol",
        dtwBox: "dtw",
        spectrumMenu: "no_psd",
        nFreqField: "n_freq",
        fourierFitBox: "fourier_fit",
        autocorrelationBox: "autocorrelation",
        windowAdaptationField: "window_adaptation",
        hitsField: "hits",
        filterTypeField: "filter_type",
        filterCutoffField: "filter_cutoff",
        filterCutoff2Field: "filter_cutoff2",
        filterOrderField: "filter_order"
    };

    console.log(nameMap);

    const params = [];
    const fields = document.querySelectorAll('#ftioParameters .ftio-params input, #ftioParameters .ftio-params select');

    fields.forEach(field => {
        const cliName = nameMap[field.name];

        if (!cliName) return;

        // Special handling for amplitude spectrum = no_psd flag
        if (field.name === "spectrumMenu") {
            if (field.value === "amplitude") {
                params.push("--no_psd");
            }
            return;
        }

        if (field.type === "checkbox") {
            if (field.checked) params.push(`--${cliName}`);
        } else if (field.value !== "" && field.value != null) {
            params.push(`--${cliName} ${field.value}`);
        }
    });

    const current = document.getElementById("cmdInput").value.trim();
    document.getElementById("cmdInput").value = params.join(" ") + (current ? " " + current : "");

}

function parseCustomArgs(argString) {
    const args = {};
    const tokens = argString.trim().split(/\s+/);

    for (let i = 0; i < tokens.length; i++) {
        if (tokens[i].startsWith("--")) {
            const key = tokens[i].replace(/^--/, "");
            const next = tokens[i + 1];

            // If next token is not another flag, treat it as value
            if (next && !next.startsWith("--")) {
                args[key] = next;
                i++;
            } else {
                // Boolean flag
                args[key] = true;
            }
        }
    }
    return args;
}

function toggleMemoryLimit() {
    const freqVal = parseFloat(document.getElementById('samplingRateField').value);
    const memGroup = document.getElementById('memoryLimitGroup');
    memGroup.style.display = (freqVal === -1) ? 'block' : 'none';
}
function toggleDecompLevel() {
    const techniqueVal = document.getElementById('techniqueMenu').value;
    const levelGroup = document.getElementById('levelGroup');
    levelGroup.style.display = (techniqueVal === 'wave_disc') ? 'block' : 'none';
}
function toggleWaveletType() {
    const techniqueVal = document.getElementById('techniqueMenu').value;
    const waveletGroup = document.getElementById('waveletGroup');
    waveletGroup.style.display = (techniqueVal === 'wave_disc' || techniqueVal === 'wave_cont') ? 'block' : 'none';
}
function toggleHitsNeeded() {
    const windowAdaptationVal = document.getElementById('windowAdaptationField').value;
    const hitsGroup = document.getElementById('hitsGroup');
    hitsGroup.style.display = (windowAdaptationVal === 'frequency_hits') ? 'block' : 'none';
}
function toggleFilter() {
    const filterTypeVal = document.getElementById('filterTypeField').value;
    const filterCutoffGroup = document.getElementById('filterCutoffGroup');
    const filterOrderGroup = document.getElementById('filterOrderGroup');
    filterCutoffGroup.style.display = (filterTypeVal !== '') ? 'block' : 'none';
    filterOrderGroup.style.display = (filterTypeVal !== '') ? 'block' : 'none';

    const filterCutoff2Field = document.getElementById('filterCutoff2Field');
    if (filterTypeVal === 'bandpass') {
        filterCutoff2Field.style.display = 'inline-block';
    } else {
        filterCutoff2Field.style.display = 'none';
    }
}

function attachFTIOEvents() {
    document.getElementById('samplingRateField').addEventListener('input', toggleMemoryLimit);
    document.getElementById('techniqueMenu').addEventListener('change', toggleDecompLevel);
    document.getElementById('techniqueMenu').addEventListener('change', toggleWaveletType);
    document.getElementById('windowAdaptationField').addEventListener('change', toggleHitsNeeded);
    document.getElementById('filterTypeField').addEventListener('change', toggleFilter);
    document.getElementById('saveBtn') ? document.getElementById('saveBtn').addEventListener('click', saveParameters) : null;
    document.getElementById('restoreBtn').addEventListener('click', populateParameters);
    document.getElementById('clearBtn').addEventListener('click', () => {
        document.getElementById('cmdInput').value = '';
    });
    document.getElementById('copyBtn').addEventListener('click', copyParameters);
}

function initFTIOTemplate() {
    attachFTIOEvents();
    populateParameters();
}