// Writes, with ROOT itself, an RNTuple of packed reals: floats and doubles
// quantized to a range (`SetQuantized`), truncated (`SetTruncated`) and in
// half precision (`SetHalfPrecision`), and a field added after four
// entries. The fixture of `tests/rntuple.rs`.
//
//   root -b -q -l 'tests/cross/root_ntuple_packed.C("root_ntuple_packed.root")'

#include <ROOT/RField.hxx>
#include <ROOT/RNTupleModel.hxx>
#include <ROOT/RNTupleWriter.hxx>

#include <memory>
#include <utility>

template <typename T>
static void add(ROOT::RNTupleModel &model, std::unique_ptr<ROOT::RField<T>> f)
{
   model.AddField(std::move(f));
}

void root_ntuple_packed(const char *path)
{
   auto model = ROOT::RNTupleModel::Create();
   {
      auto f = std::make_unique<ROOT::RField<float>>("quant");
      f->SetQuantized(20, {-2.0f, 6.0f});
      add(*model, std::move(f));
   }
   {
      auto f = std::make_unique<ROOT::RField<double>>("dquant");
      f->SetQuantized(32, {-1.0, 1.0});
      add(*model, std::move(f));
   }
   {
      auto f = std::make_unique<ROOT::RField<float>>("trunc");
      f->SetTruncated(16);
      add(*model, std::move(f));
   }
   {
      auto f = std::make_unique<ROOT::RField<float>>("half");
      f->SetHalfPrecision();
      add(*model, std::move(f));
   }
   auto writer = ROOT::RNTupleWriter::Recreate(std::move(model), "nt", path);
   auto &entry = writer->GetModel().GetDefaultEntry();
   auto quant = entry.GetPtr<float>("quant");
   auto dquant = entry.GetPtr<double>("dquant");
   auto trunc = entry.GetPtr<float>("trunc");
   auto half = entry.GetPtr<float>("half");
   std::shared_ptr<int> late;
   for (int i = 0; i < 8; ++i) {
      // A field added after four entries (a schema extension in the
      // footer): those entries read it as zero.
      if (i == 4) {
         auto updater = writer->CreateModelUpdater();
         updater->BeginUpdate();
         updater->AddField(std::make_unique<ROOT::RField<int>>("late"));
         updater->CommitUpdate();
         late = writer->GetModel().GetDefaultEntry().GetPtr<int>("late");
      }
      *quant = -2.0f + i;
      *dquant = -1.0 + 0.25 * i;
      *trunc = 1.5f * i;
      *half = 0.5f * i - 1.0f;
      if (late)
         *late = 10 * i;
      writer->Fill();
   }
}
