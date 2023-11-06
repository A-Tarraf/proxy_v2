#!/bin/sh

BUILDTEMP=$(mktemp -d)

error_out()
{
	echo "$@"
	if test -d "${BUILDTEMP}"; then
		# Cleanup all temps
		rm -fr "${BUILDTEMP}"
	fi
	exit 1
}

locate_bin()
{
	which "$@" > /dev/null 2>&1
}

assert_is_file()
{
	if test ! -f "${1}"; then
		error_out "Failed to locate file ${1}"
	fi
}

header()
{
	echo "======================="
	echo "$@"
	echo "======================="
}

check_dir()
{
	if test ! -d "$1"; then
		mkdir "$1"
	fi
}

if test $# = 1; then
	echo "Installing in  ${1}"
	PREFIX="${1}"
else
	error_out "Please provide an install prefix: $0 [PREFIX]"
fi

if test ! -d "${PREFIX}"; then
	mkdir "${PREFIX}" || error_out "Failed to create ${PREFIX} directory"
fi



header "Locate Rust Dependency"

if locate_bin "cargo"; then
	echo "Rust found in environment"
else
	error_out "Rust not found in environment"
fi

header "Build Project"


# Root of Package
SOURCE_ROOT="$(readlink -f $(dirname "$0"))"
export SOURCE_ROOT

cargo install --path "${SOURCE_ROOT}" --root "${PREFIX}" || error_out "Failed to install rust package"

# The Build Directory
BUILD_SOURCE_ROOT=""

if test -d "$SOURCE_ROOT/target/debug"; then
	BUILD_SOURCE_ROOT="$SOURCE_ROOT/target/release"
elif test -d "$SOURCE_ROOT/target/release"; then
	BUILD_SOURCE_ROOT="$SOURCE_ROOT/target/debug"
else
	error_out "Cannot locate build, did 'cargo build' succeed ?"
fi

echo "Using Build directory ${BUILD_SOURCE_ROOT}"


#
# Deploy the Client Libary and its header
#

header "Deploy Client Library"

check_dir "${PREFIX}/lib/"
check_dir "${PREFIX}/include/"

assert_is_file "${BUILD_SOURCE_ROOT}/libproxyclient.so"
cp "${BUILD_SOURCE_ROOT}/libproxyclient.so" "${PREFIX}/lib/libproxyclient.so" || error_out "Failed to install client library"

if locate_bin "cbindgen"; then
	echo "cbindgen found in path"
else
	error_out "Failed to locate cbindgen consider running 'cargo install cbindgen'"
fi

PROXY_HEADER="${PREFIX}/include/metric_proxy_client.h"

cbindgen "${SOURCE_ROOT}" -o "${PROXY_HEADER}" -l c --cpp-compat

if test -f "${PROXY_HEADER}"; then
	echo "Successfully generated proxy header in ${PROXY_HEADER}"
else
	error_out "Failed to generate proxy header see previous errors"
fi


check_dir "${PREFIX}/lib/pkgconfig/"

cat << EOF > "${PREFIX}/lib/pkgconfig/proxyclient.pc"
prefix=${PREFIX}
includedir=\${prefix}/include
libdir=\${prefix}/lib

Name: proxyclient
Description: Client library for the Metric Proxy
Version: 0.1
Cflags: -I\${includedir} -Wl,-rpath=\${libdir}
Libs: -L\${libdir} -lproxyclient -Wl,-rpath=\${libdir}
EOF

#
# Build dep detection
#
header "Detecting build dependencies"

# Detect Python

PYTHON=""

if test -z "$PYTHON"; then
	if locate_bin "python"; then
		PYTHON="python"
	elif locate_bin "python3"; then
		PYTHON="python"
	else
		error_out "Failed to locate python consider setting the PYTHON environment variable to your interpreter"
	fi
fi

echo "Using Python : ${PYTHON}"

# Detect MPICC

MPICC=""

if test -z "$MPICC"; then
	if locate_bin "mpicc"; then
		MPICC="mpicc"
	else
		error_out "Failed to locate mpicc compiler wrappers consider setting the MPICC environment variable"
	fi
fi

echo "Using MPICC : ${MPICC}"


#
# Generate the MPI Wrappers
#

header "Generating MPI Wrappers"

MPIWRAP="${SOURCE_ROOT}/exporters/mpi/dist/llnl_mpiwrap/wrap.py"
assert_is_file "$MPIWRAP"
MPI_WRAPPER_SOURCES="${SOURCE_ROOT}/exporters/mpi/mpi_wrappers.w"
assert_is_file "$MPI_WRAPPER_SOURCES"

MPI_WRAPPERS_C="${BUILDTEMP}/mpi_wrappers.c"

"${MPIWRAP}"  -f "${MPI_WRAPPER_SOURCES}" > "${MPI_WRAPPERS_C}"

if test -f "${MPI_WRAPPERS_C}"; then
	echo "Successfully generated MPI wrapper sources"
else
	echo "Failed to generate MPI wrappers see previous error"
fi

header "Compiling MPI Wrappers"

MPI_WRAPPER_LIB="${PREFIX}/lib/libmetricproxy-exporter-mpi.so"
"${MPICC}" "-I${PREFIX}/include/" "-I${SOURCE_ROOT}/exporters/mpi/" "-L${PREFIX}/lib" "-Wl,-rpath=${PREFIX}/lib" -shared -fpic "${MPI_WRAPPERS_C}" -lproxyclient -o "${MPI_WRAPPER_LIB}"

if test -f "${MPI_WRAPPER_LIB}"; then
	echo "Successfully generated MPI wrapper library"
else
	error_out "Failed to generate MPI wrappers library"
fi

#
# Deploy the Modified Strace
#
	header "Deploying Modified Strace"


	if test ! -f "${PREFIX}/bin/proxy_exporter_strace"; then

	export PKG_CONFIG_PATH="${PREFIX}/lib/pkgconfig/:$PKG_CONFIG_PATH"

	cd "${BUILDTEMP}" || error_out "Failed to move to ${BUILDTEMP}"

	"${SOURCE_ROOT}/exporters/strace/configure" "--prefix=${PREFIX}" --program-prefix=proxy_exporter_ --enable-mpers=no || error_out "Failed to configure strace"

	make install -j8 || error_out "Failed to install strace"

	echo "Sucessfully deployed"
else
	echo "Already installed"
fi

# All done if we are here
rm -fr "${BUILDTEMP}"